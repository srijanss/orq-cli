//! `POST /api/projects/{id}/features/{name}/promote` promotes a draft feature to pending, the way the popup's
//! Promote does. It is there only when remote actions are on and takes no body; a refusal is the ledger's own
//! words, and every attempt is audited as feature.promote, naming the feature.

use orq_core::remote_audit;
use orq_core::remote_inbox::{handle_with_body, ProjectsProvider, RemoteConfig, Request, RunControlOutcome};
use orq_core::storage::StorageRoots;
use serde_json::{json, Value};
use std::sync::Mutex;

struct Fake {
    asked: Mutex<Vec<String>>,
}

impl ProjectsProvider for Fake {
    fn projects(&self) -> Result<Value, String> {
        Ok(json!([]))
    }

    fn promote_feature(&self, _id: &str, feature: &str) -> Result<RunControlOutcome, String> {
        self.asked.lock().unwrap().push(feature.into());
        match feature {
            "f-draft" => Ok(RunControlOutcome::Done(feature.into())),
            "f-pending" => Ok(RunControlOutcome::Refused("feature f-pending: Pending to Pending is not allowed".into())),
            "f-broken" => Err("ledger unreadable: /home/me/code/alpha/.tdd-features.json".into()),
            _ => Ok(RunControlOutcome::Unknown),
        }
    }
}

fn roots(tag: &str) -> StorageRoots {
    let base = std::env::temp_dir().join(format!("orq-promote-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    roots
}

fn send(roots: &StorageRoots, fake: &Fake, method: &str, feature: &str, body: &str) -> (u16, Value) {
    let config = RemoteConfig { token: "tok".into() };
    let path = format!("/api/projects/p-1/features/{feature}/promote");
    let request = Request { method, path: &path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let r = handle_with_body(roots, &config, "p", "p", &request, body.as_bytes(), fake);
    (r.status, r.body)
}

fn audit(roots: &StorageRoots) -> Vec<(String, String, String)> {
    remote_audit::read(roots, 20)
        .unwrap()
        .iter()
        .rev()
        .map(|l| (l["action"].as_str().unwrap().to_string(), l["outcome"].as_str().unwrap().to_string(), l["reason"].as_str().unwrap_or("").to_string()))
        .collect()
}

#[test]
fn a_draft_is_promoted_only_with_actions_on_and_every_attempt_is_audited_as_feature_promote() {
    let roots = roots("promote");
    let fake = Fake { asked: Mutex::new(Vec::new()) };
    assert_eq!(send(&roots, &fake, "POST", "f-draft", "").0, 404, "off by default");
    assert!(fake.asked.lock().unwrap().is_empty());
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();

    assert_eq!(send(&roots, &fake, "POST", "f-draft", "{}"), (200, json!({"feature": "f-draft", "status": "pending"})));
    assert_eq!(send(&roots, &fake, "POST", "f-pending", ""), (409, json!({"error": "feature f-pending: Pending to Pending is not allowed"})));
    assert_eq!(send(&roots, &fake, "POST", "f-broken", ""), (500, json!({"error": "the feature could not be promoted"})));
    assert_eq!(send(&roots, &fake, "POST", "f-none", "").0, 404);
    assert_eq!(send(&roots, &fake, "POST", "f-draft", r#"{"status":"completed"}"#), (400, json!({"error": "the request body is not one this action takes"})));
    assert_eq!(send(&roots, &fake, "POST", "..", "").0, 404, "a name that is not a feature name is never asked about");
    assert!(matches!(send(&roots, &fake, "GET", "f-draft", "").0, 404 | 405));
    assert_eq!(*fake.asked.lock().unwrap(), ["f-draft", "f-pending", "f-broken", "f-none"]);

    let row = |o: &str, r: &str| ("feature.promote".to_string(), o.to_string(), r.to_string());
    assert_eq!(
        audit(&roots),
        [
            row("refused", "f-draft: remote actions are off"),
            row("allowed", "f-draft"),
            row("refused", "f-pending: feature f-pending: Pending to Pending is not allowed"),
            row("refused", "f-broken: the feature could not be promoted"),
            row("refused", "f-none: no such project or feature"),
            row("refused", "f-draft: bad request body"),
            row("refused", "not a feature name"),
        ]
    );
}
