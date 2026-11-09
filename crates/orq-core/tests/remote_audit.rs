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

#[test]
fn the_log_is_never_reached_through_a_symlink_and_a_loose_file_is_tightened() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let roots = roots("unsafe");
    let file = roots.state.join("remote-actions.jsonl");
    let elsewhere = roots.state.join("elsewhere.jsonl");
    fs::write(&elsewhere, "{\"action\":\"stolen\"}\n").unwrap();
    symlink(&elsewhere, &file).unwrap();
    assert!(remote_audit::record(&roots, &attempt("phone", "run.start", None, Outcome::Allowed, None), 1).is_err());
    assert!(remote_audit::read(&roots, 10).is_err(), "a link is not read through either");
    assert_eq!(fs::read_to_string(&elsewhere).unwrap(), "{\"action\":\"stolen\"}\n", "nothing was written through the link");

    fs::remove_file(&file).unwrap();
    fs::write(&file, "").unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
    remote_audit::record(&roots, &attempt("phone", "run.start", None, Outcome::Allowed, None), 2).unwrap();
    assert_eq!(fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
}

#[test]
fn a_torn_last_line_does_not_swallow_the_next_attempt_and_the_damage_is_shown() {
    let roots = roots("torn");
    remote_audit::record(&roots, &attempt("phone", "run.start", None, Outcome::Allowed, None), 1).unwrap();
    let file = roots.state.join("remote-actions.jsonl");
    let mut text = fs::read_to_string(&file).unwrap();
    text.push_str("{\"time\":2,\"identity\":\"pho"); // a write cut short, with no line end
    fs::write(&file, text).unwrap();

    remote_audit::record(&roots, &attempt("phone", "run.pause", Some("r1"), Outcome::Allowed, None), 3).unwrap();
    let lines = remote_audit::read(&roots, 10).unwrap();
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert_eq!(lines[0]["action"], "run.pause", "the new attempt is whole and visible");
    assert_eq!(lines[1], serde_json::json!({"damaged": true}), "the torn line is reported, not hidden");
    assert_eq!(lines[2]["action"], "run.start");
}

#[test]
fn a_read_looks_only_at_the_end_of_the_log_and_a_record_stays_small() {
    use std::io::Write;
    let roots = roots("bounded");
    let file = roots.state.join("remote-actions.jsonl");
    // A long stretch of old, unreadable bytes before the two real lines.
    let junk = fs::File::create(&file).unwrap();
    junk.set_len(8 * 1024 * 1024).unwrap();
    drop(junk);
    fs::OpenOptions::new().append(true).open(&file).unwrap().write_all(b"\n").unwrap();
    remote_audit::record(&roots, &attempt("phone", "run.start", None, Outcome::Allowed, None), 1).unwrap();
    remote_audit::record(&roots, &attempt("phone", "run.pause", Some("r1"), Outcome::Allowed, None), 2).unwrap();

    let lines = remote_audit::read(&roots, 3).unwrap();
    assert_eq!(lines.iter().map(|l| l["action"].as_str().unwrap_or("?").to_string()).collect::<Vec<_>>(), ["run.pause", "run.start"], "the far end of the file is never read");
    assert!(remote_audit::read(&roots, 0).unwrap().is_empty());

    let long = "x".repeat(100_000);
    remote_audit::record(&roots, &attempt(&long, "run.start", Some(&long), Outcome::Refused, Some(&long)), 3).unwrap();
    let text = fs::read(&file).unwrap();
    let last = text[..text.len() - 1].rsplit(|b| *b == b'\n').next().unwrap();
    assert!(last.len() < 4096, "one entry is {} bytes", last.len());
}

#[test]
fn a_secret_cut_by_the_length_limit_is_still_redacted() {
    let roots = roots("cut");
    let token = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
    let url_secret = "hunter2hunter2";
    let token_reason = format!("{}{token}", " ".repeat(246));
    let url_reason = format!("{}https://user:{url_secret}@example.com/x", " ".repeat(240));
    remote_audit::record(&roots, &attempt("phone", "run.start", None, Outcome::Refused, Some(&token_reason)), 1).unwrap();
    remote_audit::record(&roots, &attempt("phone", "run.start", None, Outcome::Refused, Some(&url_reason)), 2).unwrap();
    let text = fs::read_to_string(roots.state.join("remote-actions.jsonl")).unwrap();
    assert!(!text.contains("ghp_"), "{text}");
    assert!(!text.contains(url_secret) && !text.contains("user:hunter"), "{text}");
}
