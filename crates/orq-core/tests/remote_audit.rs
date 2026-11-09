//! The remote action audit log: one appended line per attempt, allowed or refused, with who, what and
//! where, and never a token. It can be read back, and nothing here edits or removes a line.

use orq_core::remote_audit::{self, Attempt, Outcome};
use orq_core::storage::StorageRoots;
use std::fs;

fn roots(tag: &str) -> StorageRoots {
    let base = std::env::temp_dir().join(format!("orq-audit-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    roots
}

fn attempt<'a>(identity: &'a str, action: &'a str, run: Option<&'a str>, outcome: Outcome, reason: Option<&'a str>) -> Attempt<'a> {
    Attempt { identity, action, project: "p-0a1b2c3d", run, outcome, reason }
}

#[test]
fn every_attempt_is_appended_with_who_what_and_where_and_read_back_newest_first() {
    let roots = roots("append");
    assert_eq!(remote_audit::read(&roots, 10).unwrap(), Vec::<serde_json::Value>::new(), "no log yet, nothing to show");
    remote_audit::record(&roots, &attempt("phone", "run.start", None, Outcome::Allowed, None), 1_000).unwrap();
    remote_audit::record(&roots, &attempt("phone", "run.pause", Some("r1"), Outcome::Refused, Some("the run is not running")), 1_001).unwrap();

    let lines = remote_audit::read(&roots, 10).unwrap();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0], serde_json::json!({"time": 1001, "identity": "phone", "action": "run.pause", "project": "p-0a1b2c3d", "run": "r1", "outcome": "refused", "reason": "the run is not running"}));
    assert_eq!(lines[1], serde_json::json!({"time": 1000, "identity": "phone", "action": "run.start", "project": "p-0a1b2c3d", "run": null, "outcome": "allowed", "reason": null}));
    assert_eq!(remote_audit::read(&roots, 1).unwrap().len(), 1, "a limit shows only the newest");
}

#[test]
fn a_token_never_reaches_the_log_and_the_file_is_private_and_only_ever_grows() {
    use std::os::unix::fs::PermissionsExt;
    let roots = roots("secret");
    let secret = "abcdef0123456789abcdef0123456789";
    let reason = format!("Authorization: Bearer {secret} was refused");
    remote_audit::record(&roots, &attempt("phone", "run.start", None, Outcome::Refused, Some(&reason)), 5).unwrap();
    // A caller-supplied line break cannot forge a second entry.
    remote_audit::record(&roots, &attempt("phone\n{\"identity\":\"admin\"}", "run.start", None, Outcome::Allowed, None), 6).unwrap();

    let file = roots.state.join("remote-actions.jsonl");
    let text = fs::read_to_string(&file).unwrap();
    assert!(!text.contains(secret), "{text}");
    assert_eq!(text.lines().count(), 2, "one line per attempt");
    assert_eq!(fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
    let first = text.lines().next().unwrap().to_string();
    remote_audit::record(&roots, &attempt("phone", "run.pause", Some("r1"), Outcome::Allowed, None), 7).unwrap();
    assert_eq!(fs::read_to_string(&file).unwrap().lines().next().unwrap(), first, "earlier lines are never rewritten");
    assert_eq!(remote_audit::read(&roots, 10).unwrap().len(), 3);
}
