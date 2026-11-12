//! The server's storage cleanup for the web: the preview is the CLI's prune plan with its totals and a hash
//! of the set it would remove; a prune plans again and removes only when the set still has that hash, so a
//! set that changed since the preview is refused and nothing is removed.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use orq_core::remote_inbox::{handle_with_body, RemoteConfig, Request};
use orq_core::retention::{Artifact, ArtifactClass, Candidate, PrunePlan};
use orq_core::storage::StorageRoots;
use orq_remote::projects::{CleanupError, StorageCleanup, StoreProjects};
use serde_json::{json, Value};

struct Fake {
    plan: Mutex<PrunePlan>,
    pruned: Mutex<Vec<PrunePlan>>,
    outcome: Mutex<Option<CleanupError>>,
}

impl Fake {
    fn new(plan: PrunePlan) -> Arc<Self> {
        Arc::new(Fake { plan: Mutex::new(plan), pruned: Mutex::default(), outcome: Mutex::default() })
    }
}

impl StorageCleanup for Fake {
    fn preview(&self) -> Result<(Vec<String>, PrunePlan), String> {
        let plan = self.plan.lock().unwrap().clone();
        let mut lines = plan.render();
        lines.push("token ghp_abcdefghijklmnopqrstuvwxyz0123 seen".to_string());
        Ok((lines, plan))
    }

    fn prune(&self, plan: &PrunePlan) -> Result<Vec<String>, CleanupError> {
        if let Some(outcome) = self.outcome.lock().unwrap().take() {
            return Err(outcome);
        }
        self.pruned.lock().unwrap().push(plan.clone());
        Ok(vec![format!("Removed {} file(s).", plan.candidates.len()), "freed by ghp_abcdefghijklmnopqrstuvwxyz0123".to_string()])
    }
}

fn file(run: &str, name: &str, bytes: u64) -> Candidate {
    Candidate { artifact: Artifact { run_id: run.into(), class: ArtifactClass::Log, rel: PathBuf::from(format!("runs/{run}/{name}")), bytes }, reason: "past its retention".into() }
}

fn plan() -> PrunePlan {
    PrunePlan { candidates: vec![file("r-1", "a.log", 1024), file("r-1", "b.log", 1024), file("r-2", "c.log", 512)], metadata: vec!["r-3".into()], kept: vec![] }
}

fn roots(tag: &str) -> StorageRoots {
    let base = orq_test_support::process_temp_dir().join(format!("orq-remote-storageclean-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();
    roots
}

fn send(roots: &StorageRoots, provider: &StoreProjects, method: &str, path: &str, body: &str) -> (u16, Value) {
    let config = RemoteConfig { token: "tok".into() };
    let request = Request { method, path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let r = handle_with_body(roots, &config, "p", "p", &request, body.as_bytes(), provider);
    (r.status, r.body)
}

fn preview(roots: &StorageRoots, provider: &StoreProjects) -> Value {
    let (status, body) = send(roots, provider, "GET", "/api/settings/storage/preview", "");
    assert_eq!(status, 200, "{body}");
    body
}

fn prune(roots: &StorageRoots, provider: &StoreProjects, hash: &str) -> (u16, Value) {
    send(roots, provider, "POST", "/api/settings/storage/prune", &json!({ "hash": hash }).to_string())
}

#[test]
fn the_preview_has_the_plan_its_totals_and_a_hash_and_a_prune_with_that_hash_removes_it() {
    let roots = roots("pruned");
    let fake = Fake::new(plan());
    let provider = StoreProjects::new(roots.clone(), roots.data.join("code")).with_storage(fake.clone());
    let shown = preview(&roots, &provider);
    assert_eq!(shown["files"], 3);
    assert_eq!(shown["bytes"], 2560);
    assert_eq!(shown["size"], "2.5 KiB");
    assert_eq!(shown["runs"], 2);
    assert_eq!(shown["metadata"], 1);
    let lines: Vec<&str> = shown["lines"].as_array().unwrap().iter().map(|l| l.as_str().unwrap()).collect();
    assert_eq!(lines[0], "3 file(s), 2.5 KiB across 2 run(s):");
    assert!(!shown.to_string().contains("ghp_"), "the lines are redacted: {shown}");
    let hash = shown["hash"].as_str().unwrap().to_string();
    assert_eq!(preview(&roots, &provider)["hash"], hash.as_str(), "the same set has the same hash");

    let (status, body) = prune(&roots, &provider, &hash);
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["lines"][0], "Removed 3 file(s).");
    assert!(!body.to_string().contains("ghp_"), "{body}");
    assert_eq!(*fake.pruned.lock().unwrap(), vec![plan()]);
}

#[test]
fn a_set_changed_since_the_preview_is_refused_and_nothing_is_removed() {
    let roots = roots("changed");
    let fake = Fake::new(plan());
    let provider = StoreProjects::new(roots.clone(), roots.data.join("code")).with_storage(fake.clone());
    let hash = preview(&roots, &provider)["hash"].as_str().unwrap().to_string();
    fake.plan.lock().unwrap().candidates.push(file("r-4", "d.log", 10));
    assert_eq!(prune(&roots, &provider, &hash).0, 409);
    let hash = preview(&roots, &provider)["hash"].as_str().unwrap().to_string();
    fake.plan.lock().unwrap().metadata.clear();
    assert_eq!(prune(&roots, &provider, &hash).0, 409, "metadata counts too");
    assert!(fake.pruned.lock().unwrap().is_empty());
}

#[test]
fn a_busy_prune_is_refused_in_its_words_and_a_failure_only_failed() {
    let roots = roots("refused");
    let fake = Fake::new(plan());
    let provider = StoreProjects::new(roots.clone(), roots.data.join("code")).with_storage(fake.clone());
    let hash = preview(&roots, &provider)["hash"].as_str().unwrap().to_string();
    *fake.outcome.lock().unwrap() = Some(CleanupError::Refused("another prune is running; try again when it has finished".into()));
    assert_eq!(prune(&roots, &provider, &hash), (409, json!({"error": "another prune is running; try again when it has finished"})));
    *fake.outcome.lock().unwrap() = Some(CleanupError::Failed("cannot open /home/someone/orq.db".into()));
    assert_eq!(prune(&roots, &provider, &hash), (500, json!({"error": "the cleanup could not be done"})));
}

#[test]
fn a_server_without_cleanup_does_not_know_it() {
    let roots = roots("none");
    let provider = StoreProjects::new(roots.clone(), roots.data.join("code"));
    assert_eq!(send(&roots, &provider, "GET", "/api/settings/storage/preview", "").0, 404);
    assert_eq!(prune(&roots, &provider, "h").0, 404);
}
