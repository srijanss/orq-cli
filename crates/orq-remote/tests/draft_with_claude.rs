//! "Draft with orq": a Todo idea, or a draft feature, is handed to an unattended, read-only claude or pi
//! session (a run of kind `plan`) that answers with a marker `{name, description, test_file, target_files,
//! depends_on}`. A Todo idea becomes a draft feature and leaves the Todo list; an improved draft waits for
//! the person's Replace or Discard. The agent here is a fake CLI that prints a marker.

mod web_deps;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use orq_core::remote_inbox::{handle_with_body, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use orq_remote::projects::StoreProjects;
use serde_json::{json, Value};

struct Setup {
    base: PathBuf,
    scan: PathBuf,
    roots: StorageRoots,
    agent: PathBuf,
}

fn setup(label: &str) -> Setup {
    let base = web_deps::private_temp_dir(label);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();
    let s = Setup { scan: base.join("code"), agent: base.join("fake-claude"), base, roots };
    answer_with(&s, &json!({"name": "dark-mode", "description": "## Problem\n\nIt is too bright.", "test_file": "tests/dark.rs", "target_files": ["src/theme.rs"], "depends_on": []}).to_string());
    s
}

/// Makes the fake agent record its arguments (NUL-separated) and print `answer`.
fn answer_with(s: &Setup, answer: &str) {
    std::fs::write(s.base.join("answer"), answer).unwrap();
    let script = format!("#!/bin/sh\nprintf '%s\\0' \"$@\" > '{args}'\ncat '{answer}'\n", args = s.base.join("args").display(), answer = s.base.join("answer").display());
    std::fs::write(&s.agent, script).unwrap();
    std::fs::set_permissions(&s.agent, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Makes the fake agent record its arguments, then wait for the file `go` before it answers.
fn answer_when_told(s: &Setup) {
    let script = format!(
        "#!/bin/sh\nprintf '%s\\0' \"$@\" > '{args}'\nwhile [ ! -e '{go}' ]; do sleep 0.05; done\ncat '{answer}'\n",
        args = s.base.join("args").display(),
        go = s.base.join("go").display(),
        answer = s.base.join("answer").display()
    );
    std::fs::write(&s.agent, script).unwrap();
    std::fs::set_permissions(&s.agent, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Starts Draft with orq on a new Todo item for project alpha, without waiting for the run to end.
fn start_todo_plan(s: &Setup) -> String {
    let item = add_todo(s, "dark mode", "Make it dark");
    let (status, started) = post(s, &format!("/api/todo/{}/draft-with-claude", item["id"]), json!({"project": "alpha", "version": item["version"], "notes": ""}));
    assert_eq!(status, 200, "{started}");
    started["run"].as_str().unwrap().to_string()
}

fn wait_for(what: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !what() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_stop_recorded_while_the_answer_waits_keeps_the_idea_and_applies_nothing() {
    let s = setup("plan-stop-race");
    let alpha = git_repo(&s.scan, "alpha");
    answer_when_told(&s);
    let run = start_todo_plan(&s);
    let store = orq_orchestrator::RunStore::new(&s.roots.state);
    // A Stop holds the run's ending while it checks the run and records it stopped.
    let ending = orq_orchestrator::run_ending_lock(&store, &run).unwrap();
    std::fs::write(s.base.join("go"), "").unwrap();
    wait_for(|| s.base.join("args").exists());
    std::thread::sleep(Duration::from_millis(500));
    orq_orchestrator::record_plan_event(&store, &run, "run.failed", json!({"reason": "stopped by the user"})).unwrap();
    drop(ending);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(finished_plan(&s, "alpha", &run)["state"], "failed");
    assert_eq!(todos(&s).len(), 1, "the idea stays on the Todo list");
    assert!(!alpha.join(".tdd-features.json").exists(), "no draft was saved");
}

#[test]
fn stop_waits_for_the_runs_ending_and_ends_a_drafting_run() {
    let s = setup("plan-stop");
    let alpha = git_repo(&s.scan, "alpha");
    answer_when_told(&s);
    let run = start_todo_plan(&s);
    wait_for(|| s.base.join("args").exists());
    let store = orq_orchestrator::RunStore::new(&s.roots.state);
    let ending = orq_orchestrator::run_ending_lock(&store, &run).unwrap();
    let stopping = {
        let (roots, scan, agent, base) = (s.roots.clone(), s.scan.clone(), s.agent.clone(), s.base.clone());
        let path = format!("/api/projects/alpha/runs/{run}/stop");
        std::thread::spawn(move || post(&Setup { base, scan, roots, agent }, &path, json!({})))
    };
    std::thread::sleep(Duration::from_millis(400));
    let (_, now) = send(&s, "GET", &format!("/api/projects/alpha/runs/{run}/plan"), "");
    assert_eq!(now["plan"]["state"], "running", "Stop waits while the ending is held");
    drop(ending);
    let (status, body) = stopping.join().unwrap();
    assert_eq!(status, 202, "{body}");
    assert_eq!(finished_plan(&s, "alpha", &run)["state"], "failed");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(todos(&s).len(), 1, "the idea stays on the Todo list");
    assert!(!alpha.join(".tdd-features.json").exists(), "no draft was saved");
}

/// The arguments the fake agent was last started with.
fn agent_args(s: &Setup) -> Vec<String> {
    let bytes = std::fs::read(s.base.join("args")).unwrap();
    bytes.split(|b| *b == 0).filter(|part| !part.is_empty()).map(|part| String::from_utf8_lossy(part).into_owned()).collect()
}

fn git_repo(root: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    std::fs::create_dir_all(&path).unwrap();
    assert!(Command::new("git").args(["init", "-q"]).current_dir(&path).status().unwrap().success());
    path
}

fn send(s: &Setup, method: &str, path: &str, body: &str) -> (u16, Value) {
    let options: std::sync::Arc<orq_remote::projects::AgentOptionsSource> =
        std::sync::Arc::new(|| vec![("claude".to_string(), vec!["opus".to_string(), "sonnet".to_string(), "haiku".to_string()]), ("pi".to_string(), vec!["openai/gpt-6.1-sol".to_string()]), ("codex".to_string(), vec!["gpt".to_string()])]);
    let mut provider = StoreProjects::new(s.roots.clone(), s.scan.clone()).with_plan_agent(s.agent.clone()).with_pi(s.base.join("fake-pi")).with_agent_options(options);
    if let Some(ms) = std::fs::read_to_string(s.base.join("timeout-ms")).ok().and_then(|ms| ms.trim().parse().ok()) {
        provider = provider.with_plan_timeout(Duration::from_millis(ms));
    }
    let config = RemoteConfig { token: "tok".into() };
    let request = Request { method, path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let r = handle_with_body(&s.roots, &config, "server-project", "server", &request, body.as_bytes(), &provider);
    (r.status, r.body)
}

fn post(s: &Setup, path: &str, body: Value) -> (u16, Value) {
    send(s, "POST", path, &body.to_string())
}

fn todos(s: &Setup) -> Vec<Value> {
    let (status, body) = send(s, "GET", "/api/todo", "");
    assert_eq!(status, 200, "{body}");
    body["todos"].as_array().unwrap().clone()
}

fn add_todo(s: &Setup, name: &str, description: &str) -> Value {
    let (status, body) = post(s, "/api/todo", json!({"name": name, "description": description}));
    assert_eq!(status, 200, "{body}");
    body["todo"].clone()
}

/// The plan run's answer once it is no longer running.
fn finished_plan(s: &Setup, project: &str, run: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let (status, body) = send(s, "GET", &format!("/api/projects/{project}/runs/{run}/plan"), "");
        assert_eq!(status, 200, "{body}");
        if body["plan"]["state"] != "running" || Instant::now() > deadline {
            return body["plan"].clone();
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn ledger(repo: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(repo.join(".tdd-features.json")).unwrap()).unwrap()
}

/// Starts Draft with orq on Todo item `item` for project alpha and waits for the run to end.
fn draft_todo(s: &Setup, item: &Value, notes: &str) -> (String, Value) {
    let path = format!("/api/todo/{}/draft-with-claude", item["id"]);
    let (status, started) = post(s, &path, json!({"project": "alpha", "version": item["version"], "notes": notes}));
    assert_eq!(status, 200, "{started}");
    let run = started["run"].as_str().unwrap().to_string();
    let plan = finished_plan(s, "alpha", &run);
    (run, plan)
}

#[test]
fn a_todo_idea_becomes_a_draft_feature_with_the_markers_fields_and_leaves_the_todo_list() {
    let s = setup("plan-todo");
    let alpha = git_repo(&s.scan, "alpha");
    let item = add_todo(&s, "dark mode", "Make it dark");
    let (_, plan) = draft_todo(&s, &item, "keep it small");
    assert_eq!(plan["state"], "completed", "{plan}");
    assert_eq!(plan["feature"], "dark-mode", "{plan}");
    assert_eq!(
        ledger(&alpha)[0],
        json!({"featureName": "dark-mode", "description": "## Problem\n\nIt is too bright.", "testFile": "tests/dark.rs", "targetFiles": ["src/theme.rs"], "status": "draft"})
    );
    assert_eq!(todos(&s), Vec::<Value>::new(), "the item went once the draft was saved");
    let prompt = agent_args(&s).join("\n");
    assert!(prompt.contains("Make it dark") && prompt.contains("keep it small"), "the idea and the notes reach the agent: {prompt}");
}

/// The value after `flag` in the fake agent's last arguments.
fn arg_after(s: &Setup, flag: &str) -> Option<String> {
    let args = agent_args(s);
    args.iter().position(|arg| arg == flag).and_then(|at| args.get(at + 1).cloned())
}

#[test]
fn a_plan_with_no_harness_or_model_runs_claude_with_haiku() {
    let s = setup("plan-default-agent");
    git_repo(&s.scan, "alpha");
    let item = add_todo(&s, "dark mode", "Make it dark");
    let (_, plan) = draft_todo(&s, &item, "");
    assert_eq!(plan["state"], "completed", "{plan}");
    assert_eq!(arg_after(&s, "--model").as_deref(), Some("haiku"), "{:?}", agent_args(&s));
}

/// Starts Draft with orq on a new Todo item for project alpha with `agent` (harness and model) and waits for the run to end.
fn draft_with(s: &Setup, agent: Value) -> Value {
    let item = add_todo(s, "dark mode", "Make it dark");
    let mut body = json!({"project": "alpha", "version": item["version"], "notes": ""});
    body.as_object_mut().unwrap().extend(agent.as_object().unwrap().clone());
    let (status, started) = post(s, &format!("/api/todo/{}/draft-with-claude", item["id"]), body);
    assert_eq!(status, 200, "{started}");
    finished_plan(s, "alpha", started["run"].as_str().unwrap())
}

#[test]
fn a_plan_uses_the_chosen_claude_model() {
    let s = setup("plan-claude-model");
    git_repo(&s.scan, "alpha");
    let plan = draft_with(&s, json!({"harness": "claude", "model": "sonnet"}));
    assert_eq!(plan["state"], "completed", "{plan}");
    assert_eq!(arg_after(&s, "--model").as_deref(), Some("sonnet"), "{:?}", agent_args(&s));
}

#[test]
fn a_pi_plan_runs_pi_read_only() {
    let s = setup("plan-pi");
    let alpha = git_repo(&s.scan, "alpha");
    // The fake pi records its arguments apart from the fake claude's, then prints the answer.
    let pi = s.base.join("fake-pi");
    let script = format!("#!/bin/sh\nprintf '%s\\0' \"$@\" > '{args}'\ncat '{answer}'\n", args = s.base.join("pi-args").display(), answer = s.base.join("answer").display());
    std::fs::write(&pi, script).unwrap();
    std::fs::set_permissions(&pi, std::fs::Permissions::from_mode(0o755)).unwrap();
    let plan = draft_with(&s, json!({"harness": "pi", "model": "openai/gpt-6.1-sol"}));
    assert_eq!(plan["state"], "completed", "{plan}");
    assert!(!s.base.join("args").exists(), "claude was not started");
    let bytes = std::fs::read(s.base.join("pi-args")).unwrap();
    let args: Vec<String> = bytes.split(|b| *b == 0).filter(|part| !part.is_empty()).map(|part| String::from_utf8_lossy(part).into_owned()).collect();
    let after = |flag: &str| args.iter().position(|arg| arg == flag).and_then(|at| args.get(at + 1).cloned());
    assert_eq!(after("--tools").as_deref(), Some("read,grep,find,ls"), "{args:?}");
    assert_eq!(after("--model").as_deref(), Some("openai/gpt-6.1-sol"), "{args:?}");
    for flag in ["--no-mcp", "--no-extensions", "--no-context-files", "--no-session"] {
        assert!(args.iter().any(|arg| arg == flag), "{flag}: {args:?}");
    }
    assert!(after("-p").is_some_and(|prompt| prompt.contains("Make it dark")), "{args:?}");
    assert_eq!(ledger(&alpha)[0]["featureName"], "dark-mode", "the marker became a draft");
    assert_eq!(ledger(&alpha)[0]["status"], "draft");
}

#[test]
fn a_pi_plan_gets_only_its_models_provider_key_of_the_servers_environment() {
    // Names no other test sets or reads, so changing this process's environment touches no other test.
    std::env::set_var("OPENAI_API_KEY", "sk-provider");
    std::env::set_var("GEMINI_API_KEY", "another-provider-not-for-pi");
    std::env::set_var("ORQ_PLAN_TEST_API_KEY", "unrelated-key-not-for-pi");
    std::env::set_var("ORQ_PLAN_TEST_SECRET", "not-for-pi");
    let s = setup("plan-pi-env");
    git_repo(&s.scan, "alpha");
    let pi = s.base.join("fake-pi");
    let script = format!("#!/bin/sh\nenv > '{env}'\ncat '{answer}'\n", env = s.base.join("pi-env").display(), answer = s.base.join("answer").display());
    std::fs::write(&pi, script).unwrap();
    std::fs::set_permissions(&pi, std::fs::Permissions::from_mode(0o755)).unwrap();
    let plan = draft_with(&s, json!({"harness": "pi", "model": "openai/gpt-6.1-sol"}));
    assert_eq!(plan["state"], "completed", "{plan}");
    let env = std::fs::read_to_string(s.base.join("pi-env")).unwrap();
    assert!(env.lines().any(|line| line == "OPENAI_API_KEY=sk-provider"), "the openai model's key reaches pi: {env}");
    assert!(!env.contains("not-for-pi"), "the rest of the server's environment does not: {env}");
}

#[test]
fn a_pi_plan_still_runs_when_an_unrelated_variable_is_not_unicode() {
    use std::os::unix::ffi::OsStrExt;
    // A name no other test sets; its value is not valid UTF-8.
    std::env::set_var("ORQ_PLAN_TEST_BYTES", std::ffi::OsStr::from_bytes(b"\xff\xfe"));
    let s = setup("plan-pi-bytes");
    let alpha = git_repo(&s.scan, "alpha");
    let pi = s.base.join("fake-pi");
    std::fs::write(&pi, format!("#!/bin/sh\ncat '{}'\n", s.base.join("answer").display())).unwrap();
    std::fs::set_permissions(&pi, std::fs::Permissions::from_mode(0o755)).unwrap();
    let plan = draft_with(&s, json!({"harness": "pi", "model": "openai/gpt-6.1-sol"}));
    assert_eq!(plan["state"], "completed", "{plan}");
    assert_eq!(ledger(&alpha)[0]["status"], "draft");
}

#[test]
fn a_harness_or_model_not_offered_is_refused() {
    let s = setup("plan-not-offered");
    git_repo(&s.scan, "alpha");
    let item = add_todo(&s, "dark mode", "Make it dark");
    for agent in [json!({"harness": "claude", "model": "gpt-9"}), json!({"harness": "codex", "model": "gpt"}), json!({"harness": "pi", "model": "sonnet"})] {
        let mut body = json!({"project": "alpha", "version": item["version"]});
        body.as_object_mut().unwrap().extend(agent.as_object().unwrap().clone());
        let (status, reply) = post(&s, &format!("/api/todo/{}/draft-with-claude", item["id"]), body);
        assert_eq!(status, 422, "{agent}: {reply}");
        assert_eq!(reply["error"], "That harness or model is not one this machine offers", "{agent}");
    }
    let alpha = s.scan.join("alpha");
    std::fs::write(alpha.join(".tdd-features.json"), r#"[{"featureName":"dark-mode","description":"old text","status":"draft"}]"#).unwrap();
    let (status, _) = post(&s, "/api/projects/alpha/features/dark-mode/draft-with-claude", json!({"harness": "claude", "model": "gpt-9"}));
    assert_eq!(status, 422, "Improve draft checks the pair too");
    let (status, _) = post(&s, "/api/projects/alpha/features/dark-mode/draft-with-claude", json!({"harness": 3}));
    assert_eq!(status, 400, "a harness that is not text is a bad body");
    assert!(!s.base.join("args").exists(), "no agent was started");
    let (_, runs) = send(&s, "GET", "/api/projects/alpha/runs", "");
    assert_eq!(runs["runs"], json!([]), "no run started: {runs}");
    assert_eq!(todos(&s).len(), 1);
}

#[test]
fn the_runs_page_reads_the_plan_with_the_run() {
    let s = setup("plan-timeline");
    git_repo(&s.scan, "alpha");
    let item = add_todo(&s, "dark mode", "Make it dark");
    let (run, plan) = draft_todo(&s, &item, "");
    let (status, body) = send(&s, "GET", &format!("/api/projects/alpha/runs/{run}"), "");
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["run"]["kind"], "plan", "{body}");
    assert_eq!(body["run"]["plan"], plan, "{body}");
}

/// Starts Improve draft on `feature` of project alpha and waits for the run to end.
fn improve(s: &Setup, feature: &str) -> (String, Value) {
    let (status, started) = post(s, &format!("/api/projects/alpha/features/{feature}/draft-with-claude"), json!({"notes": "tighter"}));
    assert_eq!(status, 200, "{started}");
    let run = started["run"].as_str().unwrap().to_string();
    let plan = finished_plan(s, "alpha", &run);
    (run, plan)
}

#[test]
fn improve_draft_proposes_beside_the_current_text_and_changes_nothing_until_replace() {
    let s = setup("plan-improve");
    let alpha = git_repo(&s.scan, "alpha");
    let ledger_text = r#"[{"featureName":"base","status":"done"},{"featureName":"dark-mode","description":"old text","status":"draft"}]"#;
    std::fs::write(alpha.join(".tdd-features.json"), ledger_text).unwrap();
    answer_with(&s, &json!({"name": "dark-mode", "description": "new text", "test_file": "tests/dark.rs", "target_files": ["src/theme.rs"], "depends_on": ["base"]}).to_string());

    let (run, plan) = improve(&s, "dark-mode");
    assert_eq!(plan["state"], "completed", "{plan}");
    assert_eq!(plan["source"], "draft", "{plan}");
    assert_eq!(plan["current"]["description"], "old text", "{plan}");
    assert_eq!(plan["proposal"]["description"], "new text", "{plan}");
    assert!(agent_args(&s).join("\n").contains("old text"), "the current draft reaches the agent");
    assert_eq!(std::fs::read_to_string(alpha.join(".tdd-features.json")).unwrap(), ledger_text, "nothing changes before Replace");

    let (status, body) = post(&s, &format!("/api/projects/alpha/runs/{run}/plan-replace"), json!({}));
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        ledger(&alpha)[1],
        json!({"featureName": "dark-mode", "description": "new text", "testFile": "tests/dark.rs", "targetFiles": ["src/theme.rs"], "dependsOn": ["base"], "status": "draft"}),
        "Replace saves the proposal and never approves it"
    );
    let (status, _) = post(&s, &format!("/api/projects/alpha/runs/{run}/plan-discard"), json!({}));
    assert_eq!(status, 409, "only one choice is taken");

    let (run, _) = improve(&s, "dark-mode");
    let before = std::fs::read_to_string(alpha.join(".tdd-features.json")).unwrap();
    let (status, body) = post(&s, &format!("/api/projects/alpha/runs/{run}/plan-discard"), json!({}));
    assert_eq!(status, 200, "{body}");
    assert_eq!(std::fs::read_to_string(alpha.join(".tdd-features.json")).unwrap(), before, "Discard changes nothing");
    assert_eq!(finished_plan(&s, "alpha", &run)["decision"], "discarded");
}

/// Project alpha with draft `dark-mode` reading "old text", and a finished Improve draft run proposing "new text".
fn improved(s: &Setup) -> (PathBuf, String) {
    let alpha = git_repo(&s.scan, "alpha");
    std::fs::write(alpha.join(".tdd-features.json"), r#"[{"featureName":"dark-mode","description":"old text","status":"draft"}]"#).unwrap();
    answer_with(s, &json!({"name": "dark-mode", "description": "new text"}).to_string());
    let (run, plan) = improve(s, "dark-mode");
    assert_eq!(plan["state"], "completed", "{plan}");
    (alpha, run)
}

#[test]
fn a_choice_waits_for_the_one_before_it_and_a_second_choice_is_refused() {
    let s = setup("plan-choose-once");
    let (alpha, run) = improved(&s);
    let store = orq_orchestrator::RunStore::new(&s.roots.state);
    let held = orq_orchestrator::run_ending_lock(&store, &run).unwrap();
    let replacing = {
        let (roots, scan, agent, base) = (s.roots.clone(), s.scan.clone(), s.agent.clone(), s.base.clone());
        let path = format!("/api/projects/alpha/runs/{run}/plan-replace");
        std::thread::spawn(move || post(&Setup { base, scan, roots, agent }, &path, json!({})))
    };
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(ledger(&alpha)[0]["description"], "old text", "Replace waits while another choice holds the run");
    drop(held);
    let (status, body) = replacing.join().unwrap();
    assert_eq!(status, 200, "{body}");
    let (status, _) = post(&s, &format!("/api/projects/alpha/runs/{run}/plan-discard"), json!({}));
    assert_eq!(status, 409, "only one choice is taken");
    assert_eq!(finished_plan(&s, "alpha", &run)["decision"], "replaced");
}

#[test]
fn a_replace_whose_choice_cannot_be_recorded_puts_the_draft_back() {
    use std::os::unix::fs::PermissionsExt;
    let s = setup("plan-replace-unrecorded");
    let (alpha, run) = improved(&s);
    let before = ledger(&alpha);
    let events = s.roots.state.join("runs").join(&run).join("events.jsonl");
    std::fs::set_permissions(&events, std::fs::Permissions::from_mode(0o444)).unwrap();
    let (status, body) = post(&s, &format!("/api/projects/alpha/runs/{run}/plan-replace"), json!({}));
    std::fs::set_permissions(&events, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_ne!(status, 200, "{body}");
    assert_eq!(ledger(&alpha), before, "the draft is as it was");
    let (status, body) = post(&s, &format!("/api/projects/alpha/runs/{run}/plan-replace"), json!({}));
    assert_eq!(status, 200, "a retry can still replace: {body}");
    assert_eq!(ledger(&alpha)[0]["description"], "new text");
}

#[test]
fn putting_a_replaced_draft_back_never_overwrites_an_edit_made_since() {
    let s = setup("plan-put-back");
    let alpha = git_repo(&s.scan, "alpha");
    let path = alpha.join(".tdd-features.json");
    let before = "[\n  {\"featureName\": \"other\", \"status\": \"draft\"},\n  {\"featureName\": \"dark-mode\", \"description\": \"old text\", \"status\": \"draft\"}\n]\n";
    std::fs::write(&path, before).unwrap();
    let loaded = orq_features::LoadedLedger::load(&path).unwrap();
    let edits = orq_features::FieldEdits { description: Some("new text".into()), ..Default::default() };
    let after = loaded.edited_fields("dark-mode", &edits).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before, "the edit is only computed");
    loaded.save(&after).unwrap();

    let replaced = orq_features::LoadedLedger::from_bytes(&path, after.clone().into_bytes()).unwrap();
    let elsewhere = after.replace("\"featureName\": \"other\", \"status\": \"draft\"", "\"featureName\": \"other\", \"description\": \"edited meanwhile\", \"status\": \"draft\"");
    std::fs::write(&path, &elsewhere).unwrap();
    assert!(replaced.save(before).is_err(), "putting back over a changed ledger is a conflict");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), elsewhere, "the edit made since is kept");

    std::fs::write(&path, &after).unwrap();
    replaced.save(before).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before, "an untouched ledger is put back byte for byte");
}

#[test]
fn the_planning_run_is_listed_in_runs_with_its_kind_and_audited() {
    let s = setup("plan-runs");
    git_repo(&s.scan, "alpha");
    let item = add_todo(&s, "dark mode", "Make it dark");
    let (run, _) = draft_todo(&s, &item, "");
    let (status, body) = send(&s, "GET", "/api/projects/alpha/runs", "");
    assert_eq!(status, 200, "{body}");
    let row = body["runs"].as_array().unwrap().iter().find(|row| row["run_id"] == run.as_str()).cloned().unwrap_or_else(|| panic!("{body}"));
    assert_eq!(row["kind"], "plan", "{row}");
    assert_eq!(row["state"], "completed", "{row}");
    // The finish line is written just after the run ends, alongside its notification.
    let deadline = Instant::now() + Duration::from_secs(10);
    let audit = loop {
        let audit = std::fs::read_to_string(s.roots.state.join("remote-actions.jsonl")).unwrap_or_default();
        if audit.contains("plan.finish") || Instant::now() > deadline {
            break audit;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(audit.contains("plan.start") && audit.contains("plan.finish"), "{audit}");
}

#[test]
fn draft_with_claude_is_refused_while_remote_actions_are_off() {
    let s = setup("plan-off");
    git_repo(&s.scan, "alpha");
    let item = add_todo(&s, "dark mode", "Make it dark");
    std::fs::write(s.roots.config_file(), "remote_actions = false\n").unwrap();
    let (status, body) = post(&s, &format!("/api/todo/{}/draft-with-claude", item["id"]), json!({"project": "alpha", "version": item["version"]}));
    assert_eq!(status, 404, "{body}");
    assert!(!s.base.join("args").exists(), "no agent was started");
    assert_eq!(todos(&s).len(), 1);
}

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const WEB_SELECTORS: [&str; 18] = [
    "[data-todo-draft]",
    "[data-plan-dialog]",
    "[data-plan-start]:disabled",
    "[data-plan-diff] [data-plan-current]",
    "[data-plan-diff] [data-plan-proposed]",
    "[data-plan-replace]",
    "[data-plan-discard]",
    "[data-improve-draft]",
    "[data-plan-project]",
    "[data-run-plan]",
    "[data-stop]",
    "[data-pause]",
    "[data-discard]",
    "[data-plan-harness] option",
    "[data-plan-model] option",
    "[data-plan-harness] option[value=\"claude\"]:checked",
    "[data-plan-model] option[value=\"haiku\"]:checked",
    "[data-plan-model] option[value=\"openai/gpt-6.1-sol\"]:checked",
];

/// The built page in jsdom with scripted replies; `None` when the web dependencies are not installed.
fn page(url: &str, responses: Vec<Value>, todo: Vec<Value>, steps: Vec<Value>) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps);
    let scenario = json!({"url": url, "responses": responses, "todo": todo, "steps": all, "selectors": WEB_SELECTORS});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn matches(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap()
}

fn shows(frame: &Value, text: &str) -> bool {
    frame["settled"]["texts"].as_array().unwrap().iter().any(|t| t.as_str() == Some(text))
}

fn web_projects() -> Value {
    json!({"status": 200, "json": {"projects": [{"id": "alpha", "name": "alpha", "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]}})
}

/// The POSTs the page sent, Todo ones included, as (url, body).
fn web_posts(out: &Value) -> Vec<(String, Value)> {
    let all = out["requests"].as_array().unwrap().iter().chain(out["todoRequests"].as_array().unwrap());
    all.filter(|r| r["method"] == "POST").map(|r| (r["url"].as_str().unwrap().to_string(), serde_json::from_str(r["body"].as_str().unwrap()).unwrap())).collect()
}

/// The harnesses and models this machine offers, codex and pi listed before claude.
fn web_options() -> Value {
    json!({"status": 200, "json": {"options": [
        {"harness": "codex", "models": ["gpt"]},
        {"harness": "pi", "models": ["openai/gpt-6.1-sol", "openai/gpt-6.1-terra"]},
        {"harness": "claude", "models": ["opus", "sonnet", "haiku"], "efforts": ["low", "high"]}
    ]}})
}

fn web_todo_item() -> Value {
    json!({"id": 7, "name": "dark mode", "description": "Make it dark", "created_at": 100, "updated_at": 100, "version": 3})
}

/// The Dashboard with one Todo item, its Draft with orq popup opened, then `steps`; the Todo POST is answered with `started`.
fn draft_popup(started: Value, steps: Vec<Value>) -> Option<Value> {
    let todo = vec![json!({"status": 200, "json": {"todos": [web_todo_item()], "remote_actions": true}}), started];
    let mut all = vec![json!({"click": "[data-todo-draft]"})];
    all.extend(steps);
    page("/dashboard", vec![web_projects(), web_options()], todo, all)
}

fn shows_selected(frame: &Value, select: &str, value: &str) -> bool {
    frame["settled"]["matches"][format!("{select} option[value=\"{value}\"]:checked")].as_u64() == Some(1)
}

#[test]
fn the_todo_button_and_dialog_say_draft_with_orq() {
    let Some(out) = draft_popup(json!({"status": 500, "json": {}}), vec![]) else { return };
    let opened = out["frames"].as_array().unwrap().last().unwrap();
    assert!(shows(opened, "Draft with orq"), "{out}");
    assert!(!shows(opened, "Draft with Claude"), "{out}");
    assert!(shows_selected(opened, "[data-plan-harness]", "claude"), "the harness starts at claude: {out}");
    assert!(shows_selected(opened, "[data-plan-model]", "haiku"), "the model starts at haiku: {out}");
    assert_eq!(matches(opened, "[data-plan-harness] option"), 2, "only claude and pi: {out}");
    assert_eq!(matches(opened, "[data-plan-model] option"), 3, "only claude's models: {out}");
    let urls: Vec<&str> = out["requests"].as_array().unwrap().iter().map(|r| r["url"].as_str().unwrap()).collect();
    assert!(urls.contains(&"/api/settings/options"), "{urls:?}");
}

#[test]
fn start_sends_the_chosen_harness_and_model() {
    let refused = json!({"status": 422, "json": {"error": "That harness or model is not one this machine offers"}});
    let steps = vec![
        json!({"click": "[data-plan-project]"}),
        json!({"click": ".feature-modal [data-project-choice=alpha]"}),
        json!({"set": "[data-plan-harness]", "value": "pi"}),
        json!({"set": "[data-plan-model]", "value": "openai/gpt-6.1-terra"}),
        json!({"click": "[data-plan-start]"}),
    ];
    let Some(out) = draft_popup(refused, steps) else { return };
    let frames = out["frames"].as_array().unwrap();
    // frames[0] is the page before any step; frames[5] follows the harness change.
    assert!(shows_selected(&frames[5], "[data-plan-model]", "openai/gpt-6.1-sol"), "pi starts at its first model: {out}");
    assert_eq!(matches(&frames[5], "[data-plan-model] option"), 2, "only pi's models: {out}");
    assert_eq!(web_posts(&out), [("/api/todo/7/draft-with-claude".to_string(), json!({"project": "alpha", "version": 3, "notes": "", "harness": "pi", "model": "openai/gpt-6.1-terra"}))], "{out}");
    let end = frames.last().unwrap();
    assert!(shows(end, "That harness or model is not one this machine offers"), "{out}");
    assert_eq!(matches(end, WEB_SELECTORS[1]), 1, "the popup stays open: {out}");
}

#[test]
fn draft_with_orq_on_a_todo_item_asks_for_a_project_and_notes_then_opens_the_run() {
    let item = web_todo_item();
    let todo = vec![json!({"status": 200, "json": {"todos": [item], "remote_actions": true}}), json!({"status": 200, "json": {"run": "plan-1-2-0", "project": "alpha"}})];
    let steps = vec![
        json!({"click": "[data-todo-draft]"}),
        json!({"set": "[data-plan-notes]", "value": "keep it small"}),
        json!({"click": "[data-plan-project]"}),
        json!({"click": ".feature-modal [data-project-choice=alpha]"}),
        json!({"click": "[data-plan-start]"}),
    ];
    let Some(out) = page("/dashboard", vec![web_projects(), web_options()], todo, steps) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert_eq!(matches(&frames[1], WEB_SELECTORS[0]), 1, "{out}");
    assert_eq!(matches(&frames[2], WEB_SELECTORS[1]), 1, "the popup opened: {out}");
    assert!(shows(&frames[2], "Make it dark"), "the idea shows: {out}");
    assert_eq!(matches(&frames[3], WEB_SELECTORS[2]), 1, "Start waits for a project: {out}");
    assert_eq!(web_posts(&out), [("/api/todo/7/draft-with-claude".to_string(), json!({"project": "alpha", "version": 3, "notes": "keep it small", "harness": "claude", "model": "haiku"}))], "{out}");
    let opened = out["requests"].as_array().unwrap().iter().any(|r| r["url"] == "/api/projects/alpha/runs/plan-1-2-0");
    assert!(opened, "the run's page opened: {out}");
}

#[test]
fn improve_draft_on_a_draft_feature_asks_for_notes_only_and_opens_the_run() {
    let ledger = json!({"status": 200, "json": {"features": [{"name": "dark-mode", "description": "old", "status": "draft", "depends_on": [], "recorded_at": null}], "remote_actions": true}});
    let runs = json!({"status": 200, "json": {"runs": [], "next_cursor": null, "total": 0, "running": 0, "remote_actions": true}});
    let started = json!({"status": 200, "json": {"run": "plan-1-2-0", "project": "alpha"}});
    let wish = json!({"status": 200, "json": {"queue_when_approved": false}});
    let steps = vec![json!({"click": "[data-improve-draft]"}), json!({"set": "[data-plan-notes]", "value": "tighter"}), json!({"click": "[data-plan-start]"})];
    let Some(out) = page("/projects/alpha/features/dark-mode", vec![ledger, runs, web_projects(), web_options(), started], vec![wish], steps) else { return };
    let frames = out["frames"].as_array().unwrap();
    let urls: Vec<&str> = out["requests"].as_array().unwrap().iter().chain(out["todoRequests"].as_array().unwrap()).map(|r| r["url"].as_str().unwrap()).collect();
    assert_eq!(matches(&frames[1], WEB_SELECTORS[7]), 1, "{out}");
    assert_eq!(matches(&frames[2], WEB_SELECTORS[1]), 1, "{out}");
    assert_eq!(matches(&frames[2], WEB_SELECTORS[8]), 0, "the project is the feature's own: {out}");
    assert!(shows(&frames[2], "Improve draft"), "{out}");
    assert_eq!(web_posts(&out), [("/api/projects/alpha/features/dark-mode/draft-with-claude".to_string(), json!({"notes": "tighter", "harness": "claude", "model": "haiku"}))], "{urls:?}");
    assert!(urls.contains(&"/api/projects/alpha/runs/plan-1-2-0"), "the run's page opened: {urls:?}");
}

#[test]
fn a_planning_run_has_its_own_badge_in_runs_and_offers_only_stop() {
    let row = |id: &str, state: &str| json!({"run_id": id, "feature": "dark-mode", "state": state, "kind": "plan", "stage": null, "started_at": null, "updated_at": null, "target_branch": ""});
    let list = json!({"status": 200, "json": {"runs": [row("plan-1-2-0", "running"), row("plan-1-2-1", "failed")], "next_cursor": null, "total": 2, "running": 1, "remote_actions": true}});
    let Some(out) = page("/projects/alpha/orchestrations", vec![list, web_projects()], vec![], vec![]) else { return };
    let end = out["frames"].as_array().unwrap().last().unwrap();
    assert_eq!(matches(end, WEB_SELECTORS[9]), 2, "{out}");
    assert!(shows(end, "Draft with orq"), "{out}");
    assert_eq!(matches(end, WEB_SELECTORS[10]), 1, "the running one can be stopped: {out}");
    assert_eq!(matches(end, WEB_SELECTORS[11]), 0, "no pause: {out}");
    assert_eq!(matches(end, WEB_SELECTORS[12]), 0, "no discard: {out}");
}

fn plan_run(plan: Value) -> Value {
    json!({"run_id": "plan-1-2-0", "feature": "dark-mode", "state": "completed", "kind": "plan", "stage": null, "started_at": null, "updated_at": null, "target_branch": "", "plan": plan})
}

fn plan_page(run: Value, more: Vec<Value>, steps: Vec<Value>) -> Option<Value> {
    let body = json!({"run": run, "events": [], "next_cursor": null, "remote_actions": true});
    let mut responses = vec![json!({"status": 200, "json": body}), web_projects(), json!({"status": 200, "stream": [], "end": "hold"})];
    responses.extend(more);
    page("/projects/alpha/orchestrations/plan-1-2-0", responses, vec![], steps)
}

fn fields(description: &str) -> Value {
    json!({"name": "dark-mode", "description": description, "test_file": "tests/dark.rs", "target_files": ["src/theme.rs"], "depends_on": []})
}

#[test]
fn improve_draft_shows_the_proposal_beside_the_current_text_and_replace_asks_the_server() {
    let plan = json!({"state": "completed", "source": "draft", "title": "dark-mode", "feature": "dark-mode", "reason": null, "proposal": fields("Proposed words"), "current": fields("Current words"), "decision": null});
    let Some(out) = plan_page(plan_run(plan), vec![json!({"status": 200, "json": {"decision": "replaced"}})], vec![json!({"click": "[data-plan-replace]"})]) else { return };
    let frames = out["frames"].as_array().unwrap();
    let shown = &frames[1];
    assert_eq!(matches(shown, WEB_SELECTORS[3]), 1, "{out}");
    assert_eq!(matches(shown, WEB_SELECTORS[4]), 1, "{out}");
    assert_eq!(matches(shown, WEB_SELECTORS[6]), 1, "Discard is offered: {out}");
    assert!(shows(shown, "Current words") && shows(shown, "Proposed words"), "{out}");
    assert_eq!(web_posts(&out), [("/api/projects/alpha/runs/plan-1-2-0/plan-replace".to_string(), json!({}))], "{out}");
}

#[test]
fn a_decided_proposal_offers_no_choice_and_a_todo_plan_links_its_new_draft() {
    let decided = json!({"state": "completed", "source": "draft", "title": "dark-mode", "feature": "dark-mode", "reason": null, "proposal": fields("Proposed words"), "current": fields("Current words"), "decision": "discarded"});
    let Some(out) = plan_page(plan_run(decided), vec![], vec![]) else { return };
    let end = out["frames"].as_array().unwrap().last().unwrap();
    assert_eq!(matches(end, WEB_SELECTORS[5]), 0, "{out}");
    assert!(shows(end, "Discarded: the draft was left as it was."), "{out}");
    let saved = json!({"state": "completed", "source": "todo", "title": "dark mode", "feature": "dark-mode", "reason": null, "proposal": fields("Proposed words"), "current": null, "decision": null});
    let out = plan_page(plan_run(saved), vec![], vec![]).unwrap();
    let end = out["frames"].as_array().unwrap().last().unwrap();
    assert!(shows(end, "Saved as the draft feature dark-mode"), "{out}");
    let failed = json!({"state": "failed", "source": "todo", "title": "dark mode", "feature": null, "reason": "the marker's name is not valid", "proposal": null, "current": null, "decision": null});
    let out = plan_page(plan_run(failed), vec![], vec![]).unwrap();
    let end = out["frames"].as_array().unwrap().last().unwrap();
    assert!(shows(end, "the marker's name is not valid"), "{out}");
}

#[test]
fn the_agent_gets_only_read_grep_and_glob_and_no_mcp_servers() {
    let s = setup("plan-tools");
    git_repo(&s.scan, "alpha");
    let item = add_todo(&s, "dark mode", "Make it dark");
    draft_todo(&s, &item, "");
    let args = agent_args(&s);
    let value_of = |flag: &str| args.iter().position(|arg| arg == flag).map(|at| args[at + 1].clone());
    assert_eq!(value_of("--tools").as_deref(), Some("Read,Grep,Glob"), "{args:?}");
    assert_eq!(value_of("--allowedTools").as_deref(), Some("Read,Grep,Glob"), "{args:?}");
    assert!(args.iter().any(|arg| arg == "--strict-mcp-config"), "{args:?}");
    assert!(!args.iter().any(|arg| arg.starts_with("--mcp-config") || arg == "--dangerously-skip-permissions"), "{args:?}");
}

#[test]
fn the_agent_runs_no_hooks_and_ignores_the_projects_settings() {
    let s = setup("plan-hooks");
    git_repo(&s.scan, "alpha");
    let item = add_todo(&s, "dark mode", "Make it dark");
    draft_todo(&s, &item, "");
    let args = agent_args(&s);
    let value_of = |flag: &str| args.iter().position(|arg| arg == flag).map(|at| args[at + 1].clone());
    let settings: Value = serde_json::from_str(&value_of("--settings").unwrap_or_default()).unwrap_or_default();
    assert_eq!(settings["disableAllHooks"], true, "{args:?}");
    assert_eq!(value_of("--setting-sources").as_deref(), Some("user"), "the project's own settings and hooks are not loaded: {args:?}");
}

/// Makes the fake agent run the shell `body` (after recording its arguments) instead of printing the answer.
fn agent_runs(s: &Setup, body: &str) {
    let script = format!("#!/bin/sh\nprintf '%s\\0' \"$@\" > '{args}'\n{body}\n", args = s.base.join("args").display());
    std::fs::write(&s.agent, script).unwrap();
    std::fs::set_permissions(&s.agent, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn a_session_that_times_out_fails_the_run_keeps_the_idea_and_the_next_run_can_start() {
    let s = setup("plan-timeout");
    let alpha = git_repo(&s.scan, "alpha");
    std::fs::write(s.base.join("timeout-ms"), "300").unwrap();
    let item = add_todo(&s, "dark mode", "Make it dark");
    agent_runs(&s, &format!("sleep 5\ncat '{}'", s.base.join("answer").display()));
    let started = Instant::now();
    let (_, plan) = draft_todo(&s, &item, "");
    assert_eq!(plan["state"], "failed", "{plan}");
    assert!(started.elapsed() < Duration::from_secs(4), "the session is ended at its timeout");
    assert!(!alpha.join(".tdd-features.json").exists(), "nothing was saved");
    assert_eq!(todos(&s).len(), 1, "the item stays");
    answer_with(&s, &json!({"name": "dark-mode", "description": "x"}).to_string());
    let (_, plan) = draft_todo(&s, &todos(&s)[0], "");
    assert_eq!(plan["state"], "completed", "the next run starts: {plan}");
}

#[test]
fn a_session_that_fails_keeps_the_idea_applies_nothing_and_the_next_run_can_start() {
    let s = setup("plan-session-fails");
    let alpha = git_repo(&s.scan, "alpha");
    let item = add_todo(&s, "dark mode", "Make it dark");
    let answer = s.base.join("answer").display().to_string();
    for (how, body) in [
        ("a nonzero exit", format!("cat '{answer}'\nexit 3")),
        ("oversized output", format!("head -c 300000 /dev/zero | tr '\\000' x\ncat '{answer}'")),
        ("a missing program", String::new()),
    ] {
        if body.is_empty() {
            std::fs::remove_file(&s.agent).unwrap();
        } else {
            agent_runs(&s, &body);
        }
        let (_, plan) = draft_todo(&s, &item, "");
        assert_eq!(plan["state"], "failed", "{how}: {plan}");
        assert!(!alpha.join(".tdd-features.json").exists(), "nothing was saved after {how}");
        assert_eq!(todos(&s).len(), 1, "the item stays after {how}");
    }
    answer_with(&s, &json!({"name": "dark-mode", "description": "x"}).to_string());
    let (_, plan) = draft_todo(&s, &item, "");
    assert_eq!(plan["state"], "completed", "the next run starts: {plan}");
}

#[test]
fn an_invalid_marker_fails_the_run_and_keeps_the_todo_item() {
    let s = setup("plan-invalid");
    let alpha = git_repo(&s.scan, "alpha");
    let item = add_todo(&s, "dark mode", "Make it dark");
    for (answer, why) in [
        (json!({"name": "Dark Mode!", "description": "x"}).to_string(), "name"),
        (json!({"name": "dark-mode", "description": "x", "depends_on": ["nope"]}).to_string(), "nope"),
        ("I could not decide.".to_string(), "no marker"),
    ] {
        answer_with(&s, &answer);
        let (_, plan) = draft_todo(&s, &item, "");
        assert_eq!(plan["state"], "failed", "{answer}: {plan}");
        assert!(plan["reason"].as_str().unwrap_or_default().contains(why), "{answer}: {plan}");
        assert!(!alpha.join(".tdd-features.json").exists(), "nothing was saved for {answer}");
        assert_eq!(todos(&s).len(), 1, "the item stays after {answer}");
    }
}
