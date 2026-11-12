//! Storage cleanup from the web: GET `/api/settings/storage/preview` says what a cleanup would remove, its
//! totals and a hash of that set; POST `/api/settings/storage/prune` `{"hash"}` removes only that set, and is
//! refused with 409 if the set has changed since the preview. Both are there only while remote actions are
//! on; the prune is audited as storage.prune, and a failure here is only said to have failed.

use std::sync::Mutex;

use orq_core::remote_audit;
use orq_core::remote_inbox::{handle_with_body, ProjectsProvider, RemoteConfig, Request, StoragePrune};
use orq_core::storage::StorageRoots;
use serde_json::{json, Value};

#[derive(Default)]
struct Fake {
    prunes: Mutex<Vec<String>>,
}

impl ProjectsProvider for Fake {
    fn projects(&self) -> Result<Value, String> {
        Ok(json!([]))
    }

    fn storage_preview(&self) -> Result<Option<Value>, String> {
        Ok(Some(json!({"lines": ["1 file(s), 2.0 KiB across 1 run(s):"], "files": 1, "bytes": 2048, "size": "2.0 KiB", "runs": 1, "metadata": 0, "hash": "h1"})))
    }

    fn storage_prune(&self, hash: &str) -> Result<Option<StoragePrune>, String> {
        self.prunes.lock().unwrap().push(hash.to_string());
        match hash {
            "h1" => Ok(Some(StoragePrune::Pruned(vec!["Removed 1 file(s), freed 2.0 KiB.".into()]))),
            "busy" => Ok(Some(StoragePrune::Refused("another prune is running; try again when it has finished".into()))),
            "broken" => Err("cannot open /home/someone/.local/share/orq/orq.db".into()),
            _ => Ok(Some(StoragePrune::Changed)),
        }
    }
}

fn roots(tag: &str, actions: bool) -> StorageRoots {
    let base = std::env::temp_dir().join(format!("orq-storageclean-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    if actions {
        std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();
    }
    roots
}

fn ask(roots: &StorageRoots, provider: &Fake, method: &str, path: &str, body: &str) -> (u16, Value) {
    let config = RemoteConfig { token: "tok".into() };
    let request = Request { method, path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let r = handle_with_body(roots, &config, "p", "p", &request, body.as_bytes(), provider);
    (r.status, r.body)
}

fn prune(roots: &StorageRoots, provider: &Fake, body: Value) -> (u16, Value) {
    ask(roots, provider, "POST", "/api/settings/storage/prune", &body.to_string())
}

fn audit(roots: &StorageRoots) -> Vec<(String, String, String)> {
    remote_audit::read(roots, 50)
        .unwrap()
        .iter()
        .rev()
        .map(|l| (l["action"].as_str().unwrap().to_string(), l["outcome"].as_str().unwrap().to_string(), l["reason"].as_str().unwrap_or("").to_string()))
        .collect()
}

fn line(outcome: &str, reason: &str) -> (String, String, String) {
    ("storage.prune".to_string(), outcome.to_string(), reason.to_string())
}

#[test]
fn the_preview_is_read_and_the_prune_removes_the_set_with_the_hash_previewed() {
    let roots = roots("pruned", true);
    let fake = Fake::default();
    let (status, preview) = ask(&roots, &fake, "GET", "/api/settings/storage/preview", "");
    assert_eq!(status, 200, "{preview}");
    assert_eq!(preview["hash"], "h1");
    assert_eq!(preview["size"], "2.0 KiB");
    assert!(audit(&roots).is_empty(), "a preview changes nothing, so it is not audited");
    assert_eq!(prune(&roots, &fake, json!({"hash": "h1"})), (200, json!({"lines": ["Removed 1 file(s), freed 2.0 KiB."]})));
    assert_eq!(*fake.prunes.lock().unwrap(), vec!["h1".to_string()]);
    assert_eq!(audit(&roots), vec![line("allowed", "Removed 1 file(s), freed 2.0 KiB.")]);
}

#[test]
fn a_changed_set_or_a_busy_prune_is_a_conflict_and_a_failure_here_only_failed() {
    let roots = roots("refused", true);
    let fake = Fake::default();
    let changed = "The cleanup changed since the preview; nothing was removed. Preview it again.";
    assert_eq!(prune(&roots, &fake, json!({"hash": "h0"})), (409, json!({"error": changed})));
    let busy = "another prune is running; try again when it has finished";
    assert_eq!(prune(&roots, &fake, json!({"hash": "busy"})), (409, json!({"error": busy})));
    assert_eq!(prune(&roots, &fake, json!({"hash": "broken"})), (500, json!({"error": "the cleanup could not be done"})));
    assert_eq!(audit(&roots), vec![line("refused", changed), line("refused", busy), line("refused", "the cleanup could not be done")]);
}

#[test]
fn the_body_must_carry_the_hash_and_nothing_else() {
    let roots = roots("body", true);
    let fake = Fake::default();
    for body in ["", "{}", r#"{"hash": ""}"#, r#"{"hash": 1}"#, r#"{"hash": "h1", "all": true}"#, "[]"] {
        assert_eq!(ask(&roots, &fake, "POST", "/api/settings/storage/prune", body).0, 400, "{body}");
    }
    assert!(fake.prunes.lock().unwrap().is_empty());
    assert_eq!(ask(&roots, &fake, "POST", "/api/settings/storage/prune", &"x".repeat(5000)).0, 413);
    assert!(audit(&roots).iter().all(|entry| *entry == line("refused", "bad request body")), "{:?}", audit(&roots));
}

#[test]
fn nothing_answers_while_remote_actions_are_off() {
    let roots = roots("off", false);
    let fake = Fake::default();
    assert_eq!(ask(&roots, &fake, "GET", "/api/settings/storage/preview", "").0, 404);
    assert_eq!(prune(&roots, &fake, json!({"hash": "h1"})).0, 404);
    assert!(fake.prunes.lock().unwrap().is_empty());
    assert_eq!(audit(&roots), vec![line("refused", "remote actions are off")]);
    // Writes other than POST are still refused.
    assert_eq!(ask(&roots, &fake, "PUT", "/api/settings/storage/prune", "{}").0, 405);
}
