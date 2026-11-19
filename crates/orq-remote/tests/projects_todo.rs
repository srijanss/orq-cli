//! The Todo list: ideas kept in orq.db, outside any project, until somebody moves one into a project as a
//! draft feature. The token (or a passkey session) reads the list; every change needs remote actions on and
//! leaves one audit line.

mod web_deps;

use std::path::{Path, PathBuf};
use std::process::Command;

use orq_core::remote_inbox::{handle_with_body, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use orq_remote::projects::StoreProjects;
use serde_json::{json, Value};

struct Setup {
    scan: PathBuf,
    roots: StorageRoots,
}

fn setup(label: &str) -> Setup {
    let base = web_deps::private_temp_dir(label);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();
    Setup { scan: base.join("code"), roots }
}

#[allow(dead_code)]
fn git_repo(root: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    std::fs::create_dir_all(&path).unwrap();
    assert!(Command::new("git").args(["init", "-q"]).current_dir(&path).status().unwrap().success());
    path
}

fn send(s: &Setup, method: &str, path: &str, body: &str) -> (u16, Value) {
    // A provider made afresh for every request, as a restarted server would.
    let provider = StoreProjects::new(s.roots.clone(), s.scan.clone());
    let config = RemoteConfig { token: "tok".into() };
    let request = Request { method, path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let r = handle_with_body(&s.roots, &config, "server-project", "server", &request, body.as_bytes(), &provider);
    (r.status, r.body)
}

fn post(s: &Setup, path: &str, body: Value) -> (u16, Value) {
    send(s, "POST", path, &body.to_string())
}

fn list(s: &Setup) -> Vec<Value> {
    let (status, body) = send(s, "GET", "/api/todo", "");
    assert_eq!(status, 200, "{body}");
    body["todos"].as_array().unwrap().clone()
}

fn names(s: &Setup) -> Vec<String> {
    list(s).iter().map(|item| item["name"].as_str().unwrap().to_string()).collect()
}

#[test]
fn added_items_are_listed_newest_first_and_survive_a_restart() {
    let s = setup("todo-add");
    assert_eq!(list(&s), Vec::<Value>::new(), "no list yet reads as empty");
    let (status, first) = post(&s, "/api/todo", json!({"name": "fix the login page", "description": "It loops.\nSometimes."}));
    assert_eq!(status, 200, "{first}");
    assert_eq!(first["todo"]["name"], "fix the login page");
    assert_eq!(post(&s, "/api/todo", json!({"name": "fix the login page"})).0, 200, "names need not be unique");
    assert_eq!(post(&s, "/api/todo", json!({"name": "Dark mode"})).0, 200);
    assert_eq!(names(&s), ["Dark mode", "fix the login page", "fix the login page"]);
    let oldest = &list(&s)[2];
    assert_eq!(oldest["description"], "It loops.\nSometimes.");
    assert_eq!(oldest["id"], first["todo"]["id"]);
    assert!(oldest["created_at"].as_u64().unwrap() > 0, "{oldest}");
    assert!(oldest["version"].is_number(), "{oldest}");
}

fn audit(s: &Setup) -> Vec<Value> {
    send(s, "GET", "/api/audit", "").1["audit"].as_array().unwrap().clone()
}

/// The audit line's action, project, outcome and reason, newest first.
fn audited(s: &Setup) -> Vec<(String, String, String, String)> {
    let text = |line: &Value, key: &str| line[key].as_str().unwrap_or_default().to_string();
    audit(s).iter().map(|line| (text(line, "action"), text(line, "project"), text(line, "outcome"), text(line, "reason"))).collect()
}

fn add(s: &Setup, name: &str, description: &str) -> Value {
    let (status, body) = post(s, "/api/todo", json!({"name": name, "description": description}));
    assert_eq!(status, 200, "{body}");
    body["todo"].clone()
}

#[test]
fn an_edit_needs_the_version_read_and_a_delete_removes_the_item_each_with_an_audit_line() {
    let s = setup("todo-edit");
    let item = add(&s, "one", "");
    let id = item["id"].as_i64().unwrap();
    let path = format!("/api/todo/{id}");
    let (status, edited) = post(&s, &path, json!({"name": "one, better", "description": "why", "version": item["version"]}));
    assert_eq!(status, 200, "{edited}");
    assert_eq!((edited["todo"]["name"].clone(), edited["todo"]["description"].clone()), (json!("one, better"), json!("why")));
    let stale = json!({"name": "mine", "description": "", "version": item["version"]});
    assert_eq!(post(&s, &path, stale), (409, json!({"error": "The item changed since you opened it; nothing was written"})));
    assert_eq!(names(&s), ["one, better"]);
    assert_eq!(post(&s, "/api/todo/999/delete", json!({})).0, 404);
    assert_eq!(post(&s, "/api/todo/999", json!({"name": "x", "description": "", "version": 1})).0, 404);
    assert_eq!(post(&s, &format!("{path}/delete"), json!({})), (200, json!({"deleted": id})));
    assert_eq!(list(&s), Vec::<Value>::new());
    let lines = audited(&s);
    assert_eq!(lines[0], ("todo.delete".into(), "".into(), "allowed".into(), "one, better".into()));
    assert_eq!(lines[1], ("todo.edit".into(), "".into(), "refused".into(), "no such item".into()));
    assert_eq!(lines[2], ("todo.delete".into(), "".into(), "refused".into(), "no such item".into()));
    assert!(lines.contains(&("todo.edit".into(), "".into(), "allowed".into(), "one, better".into())), "{lines:?}");
    assert!(lines.contains(&("todo.edit".into(), "".into(), "refused".into(), "one, better: the item changed since it was read".into())), "{lines:?}");
    assert!(lines.contains(&("todo.add".into(), "".into(), "allowed".into(), "one".into())), "{lines:?}");
}

#[test]
fn a_value_over_a_limit_is_refused_with_the_reason_and_nothing_is_written() {
    let s = setup("todo-limits");
    let refused = |why: &str| (400, json!({"error": why}));
    assert_eq!(post(&s, "/api/todo", json!({"name": ""})), refused("the name is empty"));
    assert_eq!(post(&s, "/api/todo", json!({"name": "n".repeat(201)})), refused("the name is longer than 200 characters"));
    assert_eq!(post(&s, "/api/todo", json!({"name": "n", "description": "d".repeat(20_001)})), refused("the description is longer than 20000 characters"));
    assert_eq!(post(&s, "/api/todo", json!({"name": "n", "other": 1})), (400, json!({"error": "the request body is not one this action takes"})));
    assert_eq!(post(&s, "/api/todo", json!({"name": 7})), (400, json!({"error": "the request body is not one this action takes"})));
    assert_eq!(list(&s), Vec::<Value>::new());
    // The longest description fits in a request, even when every character takes four bytes.
    let item = add(&s, &"n".repeat(200), &"\u{1f600}".repeat(20_000));
    assert_eq!(item["description"].as_str().unwrap().chars().count(), 20_000);
    for n in 1..500 {
        add(&s, &format!("item {n}"), "");
    }
    assert_eq!(post(&s, "/api/todo", json!({"name": "one more"})), refused("the Todo list holds at most 500 items"));
}

#[test]
fn with_remote_actions_off_the_list_reads_but_nothing_changes() {
    let s = setup("todo-off");
    let item = add(&s, "one", "");
    std::fs::write(s.roots.config_file(), "remote_actions = false\n").unwrap();
    let (status, body) = send(&s, "GET", "/api/todo", "");
    assert_eq!((status, body["remote_actions"].clone()), (200, json!(false)));
    let id = item["id"].as_i64().unwrap();
    assert_eq!(post(&s, "/api/todo", json!({"name": "two"})).0, 404);
    assert_eq!(post(&s, &format!("/api/todo/{id}"), json!({"name": "x", "description": "", "version": item["version"]})).0, 404);
    assert_eq!(post(&s, &format!("/api/todo/{id}/delete"), json!({})).0, 404);
    assert_eq!(post(&s, &format!("/api/todo/{id}/move"), json!({"project": "alpha", "fields": {"name": "one"}})).0, 404);
    assert_eq!(names(&s), ["one"]);
    assert_eq!(audited(&s)[0], ("todo.move".into(), "alpha".into(), "refused".into(), "one: remote actions are off".into()));
    assert_eq!(audited(&s)[3], ("todo.add".into(), "".into(), "refused".into(), "two: remote actions are off".into()));
}

#[test]
fn with_remote_actions_off_an_add_creates_no_database() {
    let s = setup("todo-off-no-db");
    std::fs::write(s.roots.config_file(), "remote_actions = false\n").unwrap();
    assert_eq!(post(&s, "/api/todo", json!({"name": "two"})).0, 404);
    assert!(!s.roots.database().exists(), "nothing was created");
    assert_eq!(audited(&s)[0], ("todo.add".into(), "".into(), "refused".into(), "two: remote actions are off".into()));
}

/// A project that saves every draft, while somebody else edits Todo item `edited` in the meantime.
struct EditedDuringSave {
    roots: StorageRoots,
    edited: i64,
}

impl orq_core::remote_inbox::ProjectsProvider for EditedDuringSave {
    fn projects(&self) -> Result<Value, String> {
        Ok(json!([]))
    }

    fn save_feature(&self, _id: &str, _feature: Option<&str>, _fields: &orq_core::remote_inbox::FeatureInput, _version: Option<&str>) -> Result<orq_core::remote_inbox::FeatureSave, String> {
        let store = orq_core::store::Store::open(&self.roots.database()).unwrap();
        let item = store.todo(self.edited).unwrap().unwrap();
        store.todo_edit(self.edited, "one, edited meanwhile", "", item.version, 1).unwrap();
        Ok(orq_core::remote_inbox::FeatureSave::Saved)
    }
}

#[test]
fn an_item_edited_while_its_move_saves_the_draft_is_kept_and_the_move_says_so() {
    let s = setup("todo-move-race");
    let item = add(&s, "one", "");
    let id = item["id"].as_i64().unwrap();
    let provider = EditedDuringSave { roots: s.roots.clone(), edited: id };
    let config = RemoteConfig { token: "tok".into() };
    let path = format!("/api/todo/{id}/move");
    let request = Request { method: "POST", path: &path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let body = json!({"project": "alpha", "fields": {"name": "one"}, "version": item["version"]}).to_string();
    let r = handle_with_body(&s.roots, &config, "server-project", "server", &request, body.as_bytes(), &provider);
    assert_eq!(r.status, 409, "{}", r.body);
    assert_eq!(names(&s), ["one, edited meanwhile"], "the newer edit is not deleted");
    assert_eq!(audited(&s)[0].2, "refused");
}

#[test]
fn with_remote_actions_off_a_change_leaves_an_older_database_unmigrated() {
    let s = setup("todo-off-old-db");
    drop(orq_core::store::Store::open(&s.roots.database()).unwrap());
    let conn = rusqlite::Connection::open(s.roots.database()).unwrap();
    conn.execute_batch("DROP TABLE queue_wishes; DROP TABLE todos").unwrap();
    conn.pragma_update(None, "user_version", 11).unwrap();
    drop(conn);
    std::fs::write(s.roots.config_file(), "remote_actions = false\n").unwrap();
    assert_eq!(post(&s, "/api/todo/1", json!({"name": "two", "description": "", "version": 1})).0, 404);
    let conn = rusqlite::Connection::open(s.roots.database()).unwrap();
    let version: u32 = conn.pragma_query_value(None, "user_version", |row| row.get(0)).unwrap();
    assert_eq!(version, 11, "a refused change migrates nothing");
    assert_eq!(audited(&s)[0], ("todo.edit".into(), "".into(), "refused".into(), "remote actions are off".into()));
}

#[test]
fn a_move_saves_a_draft_through_the_editors_path_and_only_then_removes_the_item() {
    let s = setup("todo-move");
    let alpha = git_repo(&s.scan, "alpha");
    let item = add(&s, "dark mode", "Make it dark");
    let id = item["id"].as_i64().unwrap();
    let path = format!("/api/todo/{id}/move");

    let version = item["version"].clone();
    let spaced = json!({"project": "alpha", "fields": {"name": "dark mode", "description": "Make it dark"}, "version": version});
    let rule = "is not allowed in a feature name; use letters, digits, '-', '_' or '.'";
    assert_eq!(post(&s, &path, spaced), (422, json!({"error": format!("a space at position 5 {rule} (use '-' between words)")})));
    assert_eq!(names(&s), ["dark mode"], "a refused save leaves the item");
    assert_eq!(post(&s, &path, json!({"project": "gamma", "fields": {"name": "dark-mode"}, "version": version})).0, 404);
    assert_eq!(names(&s), ["dark mode"]);

    let moved = json!({"project": "alpha", "fields": {"name": "dark-mode", "description": "Make it dark", "depends_on": []}, "version": version});
    assert_eq!(post(&s, &path, moved), (200, json!({"feature": "dark-mode", "project": "alpha"})));
    assert_eq!(list(&s), Vec::<Value>::new());
    let entries: Value = serde_json::from_slice(&std::fs::read(alpha.join(".tdd-features.json")).unwrap()).unwrap();
    assert_eq!(entries[0], json!({"featureName": "dark-mode", "description": "Make it dark", "status": "draft"}));
    assert_eq!(audited(&s)[0], ("todo.move".into(), "alpha".into(), "allowed".into(), "dark mode: dark-mode".into()));
    assert_eq!(post(&s, &path, json!({"project": "alpha", "fields": {"name": "again"}, "version": version})).0, 404, "the item is gone");
}

#[test]
fn a_move_carries_the_items_version_as_read_and_a_stale_one_moves_nothing() {
    let s = setup("todo-move-version");
    let alpha = git_repo(&s.scan, "alpha");
    let item = add(&s, "one", "");
    let id = item["id"].as_i64().unwrap();
    let read = item["version"].clone();
    let edited = post(&s, &format!("/api/todo/{id}"), json!({"name": "one better", "description": "", "version": read}));
    assert_eq!(edited.0, 200, "{edited:?}");
    let path = format!("/api/todo/{id}/move");
    let stale = json!({"project": "alpha", "fields": {"name": "one"}, "version": read});
    assert_eq!(post(&s, &path, stale), (409, json!({"error": "The item changed since you opened it; nothing was written"})));
    assert_eq!(post(&s, &path, json!({"project": "alpha", "fields": {"name": "one"}})).0, 400, "a move says which version it read");
    assert_eq!(post(&s, &path, json!({"project": "alpha", "fields": {"name": "one"}, "version": "2"})).0, 400);
    assert_eq!(names(&s), ["one better"]);
    assert!(!alpha.join(".tdd-features.json").exists(), "no draft was saved");
    assert_eq!(audited(&s)[2], ("todo.move".into(), "alpha".into(), "refused".into(), "one better: the item changed since it was read".into()));
    let current = edited.1["todo"]["version"].clone();
    let moved = json!({"project": "alpha", "fields": {"name": "one-better"}, "version": current});
    assert_eq!(post(&s, &path, moved), (200, json!({"feature": "one-better", "project": "alpha"})));
    assert_eq!(list(&s), Vec::<Value>::new());
}

fn wish(s: &Setup, feature: &str) -> Value {
    let (status, body) = send(s, "GET", &format!("/api/projects/alpha/features/{feature}/queue-wish"), "");
    assert_eq!(status, 200, "{body}");
    body["queue_when_approved"].clone()
}

fn move_wishing(s: &Setup, name: &str) {
    let item = add(s, name, "");
    let id = item["id"].as_i64().unwrap();
    let body = json!({"project": "alpha", "fields": {"name": name}, "version": item["version"], "queue_when_approved": true});
    assert_eq!(post(s, &format!("/api/todo/{id}/move"), body).0, 200);
}

fn queued(s: &Setup) -> Vec<Value> {
    let (status, body) = send(s, "GET", "/api/projects/alpha/queue", "");
    assert_eq!(status, 200, "{body}");
    body["queue"]["entries"].as_array().unwrap().iter().map(|entry| entry["feature"].clone()).collect()
}

#[test]
fn a_draft_moved_with_queue_once_approved_joins_the_queue_when_it_is_approved() {
    let s = setup("todo-wish");
    git_repo(&s.scan, "alpha");
    move_wishing(&s, "dark-mode");
    assert_eq!(wish(&s, "dark-mode"), json!(true));
    assert_eq!(queued(&s), Vec::<Value>::new(), "a draft is not queued");
    assert_eq!(post(&s, "/api/projects/alpha/features/dark-mode/promote", json!({})).0, 200);
    assert_eq!(queued(&s), [json!("dark-mode")]);
    assert_eq!(audited(&s)[0], ("queue.add".into(), "alpha".into(), "allowed".into(), "dark-mode: requested when moved from Todo".into()));
    assert_eq!(wish(&s, "dark-mode"), json!(false), "the wish is spent");
}

#[test]
fn only_a_draft_in_the_ledger_can_be_asked_to_be_queued_once_approved() {
    let s = setup("todo-wish-draft");
    git_repo(&s.scan, "alpha");
    move_wishing(&s, "dark-mode");
    assert_eq!(post(&s, "/api/projects/alpha/features/dark-mode/promote", json!({})).0, 200);
    let wished = |feature: &str| post(&s, &format!("/api/projects/alpha/features/{feature}/queue-wish"), json!({"queue_when_approved": true}));
    assert_eq!(wished("dark-mode"), (409, json!({"error": "dark-mode is not a draft in this project"})), "no longer a draft");
    assert_eq!(wished("missing"), (409, json!({"error": "missing is not a draft in this project"})));
    assert_eq!((wish(&s, "dark-mode"), wish(&s, "missing")), (json!(false), json!(false)), "nothing was kept");
    assert_eq!(audited(&s)[0], ("queue.wish".into(), "alpha".into(), "refused".into(), "missing: missing is not a draft in this project".into()));
}

#[test]
fn a_wish_can_be_cancelled_and_one_whose_draft_is_gone_is_dropped() {
    let s = setup("todo-wish-cancel");
    let alpha = git_repo(&s.scan, "alpha");
    move_wishing(&s, "dark-mode");
    let cancel = post(&s, "/api/projects/alpha/features/dark-mode/queue-wish", json!({"queue_when_approved": false}));
    assert_eq!(cancel, (200, json!({"queue_when_approved": false})));
    assert_eq!(audited(&s)[0], ("queue.wish".into(), "alpha".into(), "allowed".into(), "dark-mode: cancelled".into()));
    assert_eq!(post(&s, "/api/projects/alpha/features/dark-mode/promote", json!({})).0, 200);
    assert_eq!(queued(&s), Vec::<Value>::new());

    move_wishing(&s, "light-mode");
    // Renamed by hand in the ledger: the wish belonged to the old name.
    let ledger = alpha.join(".tdd-features.json");
    let renamed = std::fs::read_to_string(&ledger).unwrap().replace("light-mode", "lighter-mode");
    std::fs::write(&ledger, renamed).unwrap();
    assert_eq!(wish(&s, "light-mode"), json!(false));
    std::fs::write(&ledger, std::fs::read_to_string(&ledger).unwrap().replace("lighter-mode", "light-mode")).unwrap();
    assert_eq!(wish(&s, "light-mode"), json!(false), "a dropped wish does not come back with the name");
    assert_eq!(send(&s, "GET", "/api/projects/gamma/features/x/queue-wish", "").0, 404);
}
