//! What a run already knows about the commit it reports on, written down for its review report:
//! the review rounds the implement session recorded in its worktree, and the gate and CI results.

use std::path::Path;

use orq_orchestrator::CiOutcome;
use serde_json::{json, Map, Value};

/// Where the TDD tooling records each review round, and the findings still open, per review scope.
const ROUNDS_FILE: &str = ".tdd-review-rounds.json";
const FINDINGS_FILE: &str = ".tdd-review-findings.json";
/// A ledger bigger than this is not read: the tooling's own files stay far below it.
const MAX_LEDGER_BYTES: u64 = 4 * 1024 * 1024;

/// The review details (schema 1) for `head`, from the TDD tooling's review ledger in `worktree`:
/// the rounds that reviewed the run's own commits (from `base` up to `head`), oldest first, naming
/// the commit the last of them reviewed. `None` when no round of this run was recorded.
pub(crate) fn review_from_ledger(worktree: &Path, base: &str, head: &str) -> Option<Value> {
    let rounds = read_object(&worktree.join(ROUNDS_FILE))?;
    let open = read_object(&worktree.join(FINDINGS_FILE));
    let mut kept: Vec<(&str, &Map<String, Value>)> = Vec::new();
    for (scope, scope_rounds) in &rounds {
        for round in scope_rounds.as_array().into_iter().flatten().filter_map(Value::as_object) {
            let (Some(start), Some(reviewed)) = (round.get("start").and_then(Value::as_str), round.get("head").and_then(Value::as_str)) else {
                continue;
            };
            if is_commit_id(start) && is_commit_id(reviewed) && is_ancestor(worktree, base, start) && is_ancestor(worktree, reviewed, head) {
                kept.push((scope, round));
            }
        }
    }
    kept.sort_by_key(|(_, round)| (round.get("recordedAt").and_then(Value::as_str).map(str::to_string), round.get("round").and_then(Value::as_u64)));
    let last_head = kept.last()?.1.get("head").cloned();
    let rounds: Vec<Value> = kept
        .iter()
        .map(|(scope, round)| {
            let still_open = open.as_ref().and_then(|open| open.get(*scope)).and_then(Value::as_array);
            let findings: Vec<Value> = round.get("findings").and_then(Value::as_array).into_iter().flatten().map(|f| finding(f, still_open)).collect();
            json!({"findings": findings})
        })
        .collect();
    Some(json!({"schema_version": 1, "head_sha": last_head, "outcome": null, "rounds": rounds}))
}

/// The test evidence (schema 1) for `head`: the gate steps that passed for it, then what CI said
/// about it. A red check is named with exit 1; CI that timed out or could not be asked shows no
/// result, so the report does not read it as a pass; a run without CI adds nothing.
pub(crate) fn tests_evidence(head: &str, gate_steps: &[Vec<String>], ci: Option<&CiOutcome>) -> Value {
    let mut commands: Vec<Value> = gate_steps.iter().map(|argv| json!({"command": argv.join(" "), "exit_code": 0})).collect();
    let checks = |exit_code: Value| json!({"command": "CI checks", "exit_code": exit_code});
    match ci {
        None | Some(CiOutcome::Skipped) => {}
        Some(CiOutcome::Passed) => commands.push(checks(json!(0))),
        Some(CiOutcome::Failed { failing }) if failing.is_empty() => commands.push(checks(json!(1))),
        Some(CiOutcome::Failed { failing }) => commands.extend(failing.iter().map(|name| json!({"command": format!("CI check {name}"), "exit_code": 1}))),
        Some(CiOutcome::TimedOut | CiOutcome::Errored { .. }) => commands.push(checks(Value::Null)),
    }
    json!({"schema_version": 1, "head_sha": head, "commands": commands})
}

/// One finding as the report reads it. Its status is the store's: a finding the store no longer
/// holds was closed by the fix made for it, since the store keeps only live findings.
fn finding(found: &Value, still_open: Option<&Vec<Value>>) -> Value {
    let mut out = Map::new();
    for key in ["id", "severity", "file", "line", "category", "claim", "verdict"] {
        if let Some(value) = found.get(key) {
            out.insert(key.to_string(), value.clone());
        }
    }
    let status = match still_open {
        Some(open) => open.iter().find(|f| f.get("id") == found.get("id")).map_or(json!("fixed"), |f| f.get("status").cloned().unwrap_or(Value::Null)),
        None => found.get("status").cloned().unwrap_or(Value::Null),
    };
    out.insert("status".to_string(), status);
    Value::Object(out)
}

/// The JSON object in `path`, when it is a regular file of sane size. The worktree is the agent's,
/// so a symlink there is not followed out of it.
fn read_object(path: &Path) -> Option<Map<String, Value>> {
    use std::io::Read;
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.file_type().is_file() || meta.len() > MAX_LEDGER_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path).ok()?.take(MAX_LEDGER_BYTES + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > MAX_LEDGER_BYTES {
        return None;
    }
    match serde_json::from_slice(&bytes).ok()? {
        Value::Object(map) => Some(map),
        _ => None,
    }
}

/// A full commit id (SHA-1 or SHA-256), the only thing a round's ends are passed to git as.
fn is_commit_id(text: &str) -> bool {
    matches!(text.len(), 40 | 64) && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Whether `ancestor` is `descendant` or one of its ancestors.
fn is_ancestor(repo: &Path, ancestor: &str, descendant: &str) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(test)]
mod tests {
    use super::*;
    use orq_test_support::GitFixture;
    use serde_json::json;

    fn git(dir: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn commit(repo: &GitFixture, name: &str) -> String {
        std::fs::write(repo.root().join(name), name).unwrap();
        git(repo.root(), &["add", name]);
        git(repo.root(), &["-c", "user.name=T", "-c", "user.email=t@example.invalid", "commit", "-q", "-m", name]);
        git(repo.root(), &["rev-parse", "HEAD"])
    }

    fn finding(id: &str, status: &str) -> serde_json::Value {
        json!({"id": id, "severity": "medium", "file": "a.rs", "line": 3, "category": "bugs", "claim": format!("claim {id}"), "status": status})
    }

    fn round(start: &str, head: &str, number: u32, findings: &[serde_json::Value]) -> serde_json::Value {
        json!({"start": start, "head": head, "reviewer": "pi", "model": "m", "round": number, "recordedAt": format!("2026-11-07T10:0{number}:00Z"), "findings": findings})
    }

    fn ledger(repo: &GitFixture, rounds: serde_json::Value, findings: serde_json::Value) {
        std::fs::write(repo.root().join(".tdd-review-rounds.json"), rounds.to_string()).unwrap();
        std::fs::write(repo.root().join(".tdd-review-findings.json"), findings.to_string()).unwrap();
    }

    /// Rounds of the run's own commits, in order, for the last commit reviewed. A finding the
    /// store no longer holds was closed by its fix; one it holds keeps the status it has there.
    #[test]
    fn the_runs_review_rounds_become_review_details_for_the_last_reviewed_commit() {
        let repo = GitFixture::new();
        let older = commit(&repo, "older");
        let base = commit(&repo, "base");
        let first = commit(&repo, "first");
        let second = commit(&repo, "second");
        ledger(
            &repo,
            json!({
                format!("review:{older}"): [round(&older, &base, 1, &[finding("OLD", "open")])],
                format!("review:{base}"): [
                    round(&base, &second, 2, &[finding("F3", "open")]),
                    round(&base, &first, 1, &[finding("F1", "open"), finding("F2", "open")]),
                ],
            }),
            json!({
                format!("review:{older}"): [finding("OLD", "open")],
                format!("review:{base}"): [finding("F2", "rejected")],
            }),
        );

        let details = review_from_ledger(repo.root(), &base, &second).expect("the run's rounds");

        assert_eq!(details["schema_version"], 1, "{details}");
        assert_eq!(details["head_sha"], second.as_str(), "{details}");
        assert_eq!(details["outcome"], serde_json::Value::Null, "{details}");
        let statuses: Vec<Vec<(String, String)>> = details["rounds"]
            .as_array()
            .unwrap()
            .iter()
            .map(|round| round["findings"].as_array().unwrap().iter().map(|f| (f["id"].as_str().unwrap().to_string(), f["status"].as_str().unwrap().to_string())).collect())
            .collect();
        assert_eq!(
            statuses,
            vec![vec![("F1".to_string(), "fixed".to_string()), ("F2".to_string(), "rejected".to_string())], vec![("F3".to_string(), "fixed".to_string())]],
            "{details}"
        );
    }

    /// The ledger is written by the agent's tooling inside the worktree, so it is not trusted: a
    /// symlinked ledger is not followed, and a round naming something other than a commit id is
    /// not handed to git (where `--output=…` would be read as an option).
    #[test]
    fn a_symlinked_ledger_or_a_round_not_naming_commit_ids_is_not_review_evidence() {
        let repo = GitFixture::new();
        let base = commit(&repo, "base");
        let head = commit(&repo, "head");
        let elsewhere = GitFixture::new();
        let real = elsewhere.root().join("rounds.json");
        std::fs::write(&real, json!({format!("review:{base}"): [round(&base, &head, 1, &[])]}).to_string()).unwrap();
        std::os::unix::fs::symlink(&real, repo.root().join(".tdd-review-rounds.json")).unwrap();
        assert_eq!(review_from_ledger(repo.root(), &base, &head), None, "a symlinked ledger is not followed");

        std::fs::remove_file(repo.root().join(".tdd-review-rounds.json")).unwrap();
        let marker = repo.root().join("written-by-git");
        ledger(
            &repo,
            json!({"review:x": [round(&format!("--output={}", marker.display()), &head, 1, &[]), round(&base, "HEAD", 2, &[])]}),
            json!({}),
        );
        assert_eq!(review_from_ledger(repo.root(), &base, &head), None, "only commit ids are rounds of this run");
        assert!(!marker.exists(), "a round's start was passed to git as an option");
    }

    /// The gate steps that passed for the head, then what CI said about it: each red check by
    /// name, no result while CI is undecided, and nothing at all when the run has no CI.
    #[test]
    fn test_evidence_names_the_head_and_holds_the_gate_steps_and_the_ci_verdict() {
        let steps = vec![vec!["cargo".to_string(), "test".to_string()], vec!["true".to_string()]];
        let commands = |ci: Option<&CiOutcome>| tests_evidence("abc123", &steps, ci)["commands"].clone();

        let evidence = tests_evidence("abc123", &steps, None);
        assert_eq!(evidence["schema_version"], 1, "{evidence}");
        assert_eq!(evidence["head_sha"], "abc123", "{evidence}");
        let gate = json!([{"command": "cargo test", "exit_code": 0}, {"command": "true", "exit_code": 0}]);
        assert_eq!(evidence["commands"], gate);
        assert_eq!(commands(Some(&CiOutcome::Skipped)), gate);
        assert_eq!(commands(Some(&CiOutcome::Passed))[2], json!({"command": "CI checks", "exit_code": 0}));
        assert_eq!(
            commands(Some(&CiOutcome::Failed { failing: vec!["test".to_string(), "lint".to_string()] })).as_array().unwrap()[2..],
            [json!({"command": "CI check test", "exit_code": 1}), json!({"command": "CI check lint", "exit_code": 1})]
        );
        assert_eq!(commands(Some(&CiOutcome::Failed { failing: vec![] }))[2], json!({"command": "CI checks", "exit_code": 1}));
        assert_eq!(commands(Some(&CiOutcome::TimedOut))[2], json!({"command": "CI checks", "exit_code": null}));
        assert_eq!(commands(Some(&CiOutcome::Errored { reason: "down".to_string() }))[2], json!({"command": "CI checks", "exit_code": null}));
    }
}
