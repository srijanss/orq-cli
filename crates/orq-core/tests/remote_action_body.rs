//! Action routes take an optional JSON body: nothing or `{}` acts as before, anything else is refused
//! before the action runs, a body over the cap is refused as too large, and every refusal is audited.

use orq_core::remote_audit;
use orq_core::remote_inbox::{handle_with_body, ProjectsProvider, RemoteConfig, Request, RunControl, RunControlOutcome, RunStart, MAX_ACTION_BODY};
use orq_core::storage::StorageRoots;
use serde_json::{json, Value};
use std::sync::Mutex;

#[derive(Default)]
struct Fake {
    asked: Mutex<Vec<String>>,
}

impl ProjectsProvider for Fake {
    fn projects(&self) -> Result<Value, String> {
        Ok(json!([]))
    }

    fn start_run(&self, _id: &str, feature: &str) -> Result<RunStart, String> {
        self.asked.lock().unwrap().push(format!("start {feature}"));
        Ok(RunStart::Started("run-1".into()))
    }

    fn control_run(&self, _id: &str, run: &str, _control: RunControl) -> Result<RunControlOutcome, String> {
        self.asked.lock().unwrap().push(format!("control {run}"));
        Ok(RunControlOutcome::Done(run.into()))
    }
}

fn roots(tag: &str) -> StorageRoots {
    let base = orq_test_support::process_temp_dir().join(format!("orq-actionbody-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    roots
}

fn post(roots: &StorageRoots, fake: &Fake, path: &str, body: &[u8]) -> (u16, Value) {
    let config = RemoteConfig { token: "tok".into() };
    let request = Request { method: "POST", path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let r = handle_with_body(roots, &config, "p", "p", &request, body, fake);
    (r.status, r.body)
}

#[test]
fn an_empty_or_object_body_acts_and_any_other_body_is_refused_before_the_action_runs() {
    let roots = roots("body");
    let fake = Fake::default();
    let start = "/api/projects/p-1/features/next-one/runs";
    let pause = "/api/projects/p-1/runs/r-1/pause";

    // With actions off a bad body still finds no route: nothing tells a caller the route exists.
    assert_eq!(post(&roots, &fake, pause, b"[1]").0, 404);
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();

    assert_eq!(post(&roots, &fake, start, b"").0, 202);
    assert_eq!(post(&roots, &fake, start, b"{}").0, 202);
    assert_eq!(post(&roots, &fake, pause, b" { } ").0, 202);
    assert_eq!(fake.asked.lock().unwrap().len(), 3);

    let refused = json!({"error": "the request body is not one this action takes"});
    for body in [&b"not json"[..], b"[]", b"\"x\"", b"null", b"{\"run\": \"other\"}", b"{} {}"] {
        assert_eq!(post(&roots, &fake, start, body), (400, refused.clone()), "{}", String::from_utf8_lossy(body));
        assert_eq!(post(&roots, &fake, pause, body), (400, refused.clone()), "{}", String::from_utf8_lossy(body));
    }

    let mut large = vec![b' '; MAX_ACTION_BODY];
    large.extend_from_slice(b"{}");
    assert_eq!(post(&roots, &fake, pause, &large), (413, json!({"error": "the request body is too large"})));
    assert_eq!(fake.asked.lock().unwrap().len(), 3, "no refused body reached the provider");

    let log = remote_audit::read(&roots, 100).unwrap();
    let bad: Vec<&Value> = log.iter().filter(|e| e["reason"] == "bad request body").collect();
    assert_eq!(bad.len(), 13, "{log:?}");
    assert!(bad.iter().all(|e| e["outcome"] == "refused"));
    assert!(bad.iter().any(|e| e["action"] == "run.start") && bad.iter().any(|e| e["action"] == "run.pause"));
}
