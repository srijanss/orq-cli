//! The web's feature editor saves through POST `/api/projects/{id}/features` (add) and
//! POST `/api/projects/{id}/features/{name}` (edit), with JSON `{fields, version}`. The version is the one the
//! fields were read with, so a ledger changed since is a conflict and nothing is overwritten. Saved is 200,
//! refused values 422, a conflict 409, a failure here 500 with a generic message; every attempt is audited
//! as feature.add or feature.edit, and nothing answers while remote actions are off.

use std::sync::Mutex;

use orq_core::remote_audit;
use orq_core::remote_inbox::{handle_with_body, FeatureInput, FeatureSave, ProjectsProvider, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use serde_json::{json, Value};

#[derive(Default)]
struct Fake {
    saves: Mutex<Vec<(String, Option<String>, FeatureInput, Option<String>)>>,
}

impl ProjectsProvider for Fake {
    fn projects(&self) -> Result<Value, String> {
        Ok(json!([]))
    }

    fn save_feature(&self, id: &str, feature: Option<&str>, fields: &FeatureInput, version: Option<&str>) -> Result<FeatureSave, String> {
        self.saves.lock().unwrap().push((id.to_string(), feature.map(str::to_string), fields.clone(), version.map(str::to_string)));
        if id != "p-1" {
            return Ok(FeatureSave::Unknown);
        }
        match (feature, fields.description.as_deref(), version) {
            (Some("f-gone"), _, _) => Ok(FeatureSave::Unknown),
            (_, Some("broken"), _) => Err("could not write /home/someone/ledger.json".into()),
            (_, Some("bad"), _) => Ok(FeatureSave::Invalid("dependsOn names an unknown feature f-x".into())),
            (_, _, Some("old")) => Ok(FeatureSave::Conflict("The ledger changed since you opened it; nothing was written".into())),
            _ => Ok(FeatureSave::Saved),
        }
    }
}

fn roots(tag: &str, actions: bool) -> StorageRoots {
    let base = orq_test_support::process_temp_dir().join(format!("orq-featuresave-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    if actions {
        std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();
    }
    roots
}

fn post(roots: &StorageRoots, provider: &Fake, path: &str, body: Value) -> (u16, Value) {
    let config = RemoteConfig { token: "tok".into() };
    let request = Request { method: "POST", path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let body = body.to_string();
    let r = handle_with_body(roots, &config, "p", "p", &request, body.as_bytes(), provider);
    (r.status, r.body)
}

fn audit(roots: &StorageRoots) -> Vec<(String, String, String)> {
    remote_audit::read(roots, 50)
        .unwrap()
        .iter()
        .rev()
        .map(|l| (l["action"].as_str().unwrap().to_string(), l["outcome"].as_str().unwrap().to_string(), l["reason"].as_str().unwrap_or("").to_string()))
        .collect()
}

fn line(action: &str, outcome: &str, reason: &str) -> (String, String, String) {
    (action.to_string(), outcome.to_string(), reason.to_string())
}

#[test]
fn an_add_and_an_edit_are_saved_with_the_fields_and_version_sent_and_audited() {
    let roots = roots("saved", true);
    let fake = Fake::default();
    let add = json!({"fields": {"name": "f-new", "description": "New", "depends_on": ["f-1"], "parallelizable": true, "test_file": "tests/new.rs", "target_files": ["src/new.rs"]}, "version": "v1"});
    assert_eq!(post(&roots, &fake, "/api/projects/p-1/features", add), (200, json!({"feature": "f-new"})));
    let edit = json!({"fields": {"name": "f-1", "description": "Reworded", "target_files": ["src/a.rs"]}, "version": "v1"});
    assert_eq!(post(&roots, &fake, "/api/projects/p-1/features/f-1", edit), (200, json!({"feature": "f-1"})));
    // An add may leave the version out: appending never overwrites what is there.
    assert_eq!(post(&roots, &fake, "/api/projects/p-1/features", json!({"fields": {"name": "f-other"}})).0, 200);

    let saves = fake.saves.lock().unwrap();
    let added = FeatureInput {
        name: Some("f-new".into()),
        description: Some("New".into()),
        depends_on: Some(vec!["f-1".into()]),
        parallelizable: Some(true),
        test_file: Some("tests/new.rs".into()),
        target_files: Some(vec!["src/new.rs".into()]),
    };
    assert_eq!(saves[0], ("p-1".into(), None, added, Some("v1".into())));
    let edited = FeatureInput { name: Some("f-1".into()), description: Some("Reworded".into()), target_files: Some(vec!["src/a.rs".into()]), ..FeatureInput::default() };
    assert_eq!(saves[1], ("p-1".into(), Some("f-1".into()), edited, Some("v1".into())));
    assert_eq!(saves[2].3, None);
    assert_eq!(audit(&roots), [line("feature.add", "allowed", "f-new"), line("feature.edit", "allowed", "f-1"), line("feature.add", "allowed", "f-other")]);
}

#[test]
fn refused_values_a_conflict_a_failure_and_an_unknown_feature_are_answered_and_audited() {
    let roots = roots("refused", true);
    let fake = Fake::default();
    let edit = |description: &str, version: &str| json!({"fields": {"description": description}, "version": version});
    assert_eq!(post(&roots, &fake, "/api/projects/p-1/features/f-1", edit("bad", "v1")), (422, json!({"error": "dependsOn names an unknown feature f-x"})));
    assert_eq!(post(&roots, &fake, "/api/projects/p-1/features/f-1", edit("fine", "old")), (409, json!({"error": "The ledger changed since you opened it; nothing was written"})));
    assert_eq!(post(&roots, &fake, "/api/projects/p-1/features/f-1", edit("broken", "v1")), (500, json!({"error": "the feature could not be saved"})), "a path on this machine is never shown");
    assert_eq!(post(&roots, &fake, "/api/projects/p-1/features/f-gone", edit("fine", "v1")).0, 404);
    assert_eq!(post(&roots, &fake, "/api/projects/p-2/features", json!({"fields": {"name": "f-new"}})).0, 404);
    assert_eq!(
        audit(&roots),
        [
            line("feature.edit", "refused", "f-1: dependsOn names an unknown feature f-x"),
            line("feature.edit", "refused", "f-1: The ledger changed since you opened it; nothing was written"),
            line("feature.edit", "refused", "f-1: the feature could not be saved"),
            line("feature.edit", "refused", "f-gone: no such project or feature"),
            line("feature.add", "refused", "f-new: no such project or feature"),
        ]
    );
}

#[test]
fn a_body_that_is_not_a_save_is_refused_before_the_ledger_is_touched() {
    let roots = roots("body", true);
    let fake = Fake::default();
    let bad = [
        ("/api/projects/p-1/features", json!({})),
        ("/api/projects/p-1/features", json!({"fields": {"name": ""}})),
        ("/api/projects/p-1/features", json!({"fields": {"name": "f-new", "status": "pending"}})),
        ("/api/projects/p-1/features", json!({"fields": {"name": "f-new"}, "extra": 1})),
        ("/api/projects/p-1/features", json!({"fields": {"name": "f-new", "depends_on": "f-1"}})),
        ("/api/projects/p-1/features", json!({"fields": {"name": "f-new", "parallelizable": "yes"}})),
        ("/api/projects/p-1/features/f-1", json!({"fields": {"description": "No version"}})),
        ("/api/projects/p-1/features/f-1", json!({"fields": {"name": "f-renamed"}, "version": "v1"})),
        ("/api/projects/p-1/features/f-1", json!({"fields": [], "version": "v1"})),
    ];
    for (path, body) in bad {
        assert_eq!(post(&roots, &fake, path, body.clone()).0, 400, "{path} {body}");
    }
    assert!(fake.saves.lock().unwrap().is_empty());
    assert!(audit(&roots).iter().all(|(_, outcome, reason)| outcome == "refused" && reason.ends_with("bad request body")), "{:?}", audit(&roots));
    assert_eq!(post(&roots, &fake, "/api/projects/p-1/features/..%2Fx", json!({"fields": {}, "version": "v1"})).0, 404);
}

#[test]
fn with_actions_off_nothing_is_saved_and_the_attempt_is_audited() {
    let roots = roots("off", false);
    let fake = Fake::default();
    assert_eq!(post(&roots, &fake, "/api/projects/p-1/features", json!({"fields": {"name": "f-new"}})).0, 404);
    assert_eq!(post(&roots, &fake, "/api/projects/p-1/features/f-1", json!({"fields": {"description": "x"}, "version": "v1"})).0, 404);
    assert!(fake.saves.lock().unwrap().is_empty());
    assert_eq!(audit(&roots), [line("feature.add", "refused", "remote actions are off"), line("feature.edit", "refused", "f-1: remote actions are off")]);
}
