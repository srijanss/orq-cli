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

/// The review details (schema 1) from the TDD tooling's review ledger in `worktree`: the rounds
/// that reviewed the run's own commits (made on top of `base`), oldest first, naming the commit
/// the last of them reviewed. The report checks that commit against the one it reports on, so a
/// round of a commit a rebase or a later fix replaced shows as review of another commit.
/// `None` when no round of this run was recorded.
pub(crate) fn review_from_ledger(worktree: &Path, base: &str) -> Option<Value> {
    review_from_ledger_with(worktree, base, |ancestor, descendant| is_ancestor(worktree, ancestor, descendant))
}

/// [`review_from_ledger`] asking `check` whether one commit is an ancestor of another. Rounds of
/// one review share their ends, so each distinct pair is asked about once.
fn review_from_ledger_with(worktree: &Path, base: &str, mut check: impl FnMut(&str, &str) -> bool) -> Option<Value> {
    let rounds = read_object(&worktree.join(ROUNDS_FILE))?;
    let open = read_object(&worktree.join(FINDINGS_FILE));
    let mut known: std::collections::HashMap<(String, String), bool> = std::collections::HashMap::new();
    let mut is_ancestor = |ancestor: &str, descendant: &str| *known.entry((ancestor.to_string(), descendant.to_string())).or_insert_with(|| check(ancestor, descendant));
    let mut kept: Vec<(&str, &Map<String, Value>)> = Vec::new();
    for (scope, scope_rounds) in &rounds {
        for round in scope_rounds.as_array().into_iter().flatten().filter_map(Value::as_object) {
            let (Some(start), Some(reviewed)) = (round.get("start").and_then(Value::as_str), round.get("head").and_then(Value::as_str)) else {
                continue;
            };
            if is_commit_id(start) && is_commit_id(reviewed) && is_ancestor(base, start) && is_ancestor(start, reviewed) {
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

/// The latest report build of a run to start, shared by that run's builds.
type Latest = std::sync::Arc<std::sync::Mutex<u64>>;

/// A report build's place among its run's builds, taken when the build starts. A build detached
/// on a timeout can finish after the build that followed it; its turn keeps it from writing over
/// what that later build wrote, without the later build waiting for it.
pub(crate) struct ReportTurn {
    generation: u64,
    latest: Latest,
}

/// The next turn for a report build of `run_id`; from now on, earlier builds of the run write nothing.
pub(crate) fn report_turn(run_id: &str) -> ReportTurn {
    static RUNS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, Latest>>> = std::sync::OnceLock::new();
    let mut runs = RUNS.get_or_init(Default::default).lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let latest = runs.entry(run_id.to_string()).or_default().clone();
    drop(runs);
    let mut taken = latest.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    *taken += 1;
    let generation = *taken;
    drop(taken);
    ReportTurn { generation, latest }
}

impl ReportTurn {
    /// Runs `write`, one of the build's writes of the run's evidence or report, unless a later build
    /// of the run has started. The check and the write are one step, so no later build's write
    /// can come between them; keep `write` to local file writes, since later builds wait for it.
    pub(crate) fn write<T>(&self, write: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        let latest = self.latest.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if *latest != self.generation {
            return Err("a later build of this run's report has started".to_string());
        }
        let written = write();
        drop(latest);
        written
    }
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
/// so a symlink there is not followed out of it, and a pipe there is not waited on. It is opened
/// once and checked through that descriptor, so it cannot be swapped between the check and the read.
fn read_object(path: &Path) -> Option<Map<String, Value>> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > MAX_LEDGER_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    file.by_ref().take(MAX_LEDGER_BYTES + 1).read_to_end(&mut bytes).ok()?;
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

        let details = review_from_ledger(repo.root(), &base).expect("the run's rounds");

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

    /// A report build that timed out can finish after the one that followed it (the build before
    /// CI, after the build with CI's verdict). Once a later build of the run started, the earlier
    /// one writes nothing more, so the CI result stays; another run's builds are not affected.
    #[test]
    fn a_report_build_that_finishes_after_a_later_one_started_does_not_overwrite_it() {
        let run = "run-overlapping-builds";
        let before_ci = report_turn(run);
        assert_eq!(before_ci.write(|| Ok("evidence before CI")), Ok("evidence before CI"), "a build writes while it is the latest");
        let with_ci = report_turn(run);
        let other_run = report_turn("run-overlapping-builds-other");

        assert_eq!(with_ci.write(|| Ok("with CI")), Ok("with CI"));
        let mut late_write_ran = false;
        let late = before_ci.write(|| {
            late_write_ran = true;
            Ok("report before CI")
        });
        assert!(late.is_err(), "{late:?}");
        assert!(!late_write_ran, "the superseded build does not write the run's evidence or report");
        assert_eq!(other_run.write(|| Ok("other")), Ok("other"), "another run's builds are not held back");
    }

    /// A build that stalls between its writes (on git, say) holds nothing, so the build that
    /// follows it writes without waiting for it.
    #[test]
    fn a_stalled_report_build_does_not_hold_back_the_one_that_follows_it() {
        let run = "run-stalled-build";
        let stalled = report_turn(run);
        let (stall_tx, stall_rx) = std::sync::mpsc::channel::<()>();
        let (wrote_tx, wrote_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let first = stalled.write(|| Ok("evidence"));
            wrote_tx.send(()).unwrap();
            stall_rx.recv().unwrap();
            (first, stalled.write(|| Ok("report")))
        });
        wrote_rx.recv_timeout(std::time::Duration::from_secs(5)).expect("the first build wrote its evidence");

        let following = report_turn(run);
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || done_tx.send(following.write(|| Ok("with CI"))).unwrap());
        assert_eq!(done_rx.recv_timeout(std::time::Duration::from_secs(5)), Ok(Ok("with CI")), "the following build is not held back");

        stall_tx.send(()).unwrap();
        let (first, late) = worker.join().unwrap();
        assert_eq!(first, Ok("evidence"));
        assert!(late.is_err(), "{late:?}");
    }

    /// Many rounds of one review share their start and head, so git is asked about each distinct
    /// pair of commits once, not once per round.
    #[test]
    fn each_distinct_pair_of_commits_is_checked_for_ancestry_once() {
        let repo = GitFixture::new();
        let base = commit(&repo, "base");
        let head = commit(&repo, "head");
        let rounds: Vec<serde_json::Value> = (1..=5).map(|n| round(&base, &head, n, &[])).collect();
        ledger(&repo, json!({format!("review:{base}"): rounds}), json!({}));

        let mut asked: Vec<(String, String)> = Vec::new();
        let details = review_from_ledger_with(repo.root(), &base, |ancestor, descendant| {
            asked.push((ancestor.to_string(), descendant.to_string()));
            is_ancestor(repo.root(), ancestor, descendant)
        })
        .expect("the run's rounds");

        assert_eq!(details["rounds"].as_array().map(Vec::len), Some(5), "{details}");
        assert_eq!(asked, vec![(base.clone(), base.clone()), (base.clone(), head.clone())]);
    }

    /// A rebase rewrites the commits the session reviewed, so the pushed head no longer holds them.
    /// The rounds are still the run's: they name the commit they reviewed, and the report shows
    /// them as review of another commit rather than as no review at all.
    #[test]
    fn rounds_of_commits_a_rebase_rewrote_still_name_the_commit_they_reviewed() {
        let repo = GitFixture::new();
        let base = commit(&repo, "base");
        let reviewed = commit(&repo, "reviewed");
        git(repo.root(), &["reset", "-q", "--hard", &base]);
        commit(&repo, "target");
        commit(&repo, "rebased");
        ledger(&repo, json!({format!("review:{base}"): [round(&base, &reviewed, 1, &[])]}), json!({}));

        let details = review_from_ledger(repo.root(), &base).expect("the rounds of the rewritten commits");

        assert_eq!(details["head_sha"], reviewed.as_str(), "{details}");
        assert_eq!(details["rounds"].as_array().map(Vec::len), Some(1), "{details}");
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
        assert_eq!(review_from_ledger(repo.root(), &base), None, "a symlinked ledger is not followed");

        std::fs::remove_file(repo.root().join(".tdd-review-rounds.json")).unwrap();
        let marker = repo.root().join("written-by-git");
        ledger(
            &repo,
            json!({"review:x": [round(&format!("--output={}", marker.display()), &head, 1, &[]), round(&base, "HEAD", 2, &[])]}),
            json!({}),
        );
        assert_eq!(review_from_ledger(repo.root(), &base), None, "only commit ids are rounds of this run");
        assert!(!marker.exists(), "a round's start was passed to git as an option");
    }

    /// A ledger the agent replaced with a named pipe is not review evidence, and reading it does not
    /// wait for a writer that never comes.
    #[test]
    fn a_ledger_that_is_a_named_pipe_is_not_read() {
        let repo = GitFixture::new();
        let base = commit(&repo, "base");
        let made = std::process::Command::new("mkfifo").arg(repo.root().join(".tdd-review-rounds.json")).status().unwrap();
        assert!(made.success());
        let (sender, receiver) = std::sync::mpsc::channel();
        let root = repo.root().to_path_buf();
        std::thread::spawn(move || sender.send(review_from_ledger(&root, &base)).unwrap());

        let details = receiver.recv_timeout(std::time::Duration::from_secs(5)).expect("reading the ledger blocked");

        assert_eq!(details, None);
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
