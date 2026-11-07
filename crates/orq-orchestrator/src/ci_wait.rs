//! The CI-wait stage: after the review request exists, poll the provider for the
//! checks of the pushed head SHA until they finish or a timeout passes, and record
//! the result. Only read-only API calls are made. Nothing that is not positively
//! green counts as green: a missing, malformed or unrecognised answer is recorded
//! as an error, never as a pass.

use std::path::Path;
use std::time::{Duration, Instant};

use crate::implement::{next_event_id, now_rfc3339};
use crate::review_draft::{run_drafter, DraftSession};
use crate::review_request::{provider_environment, repository_url};
use crate::store::{Event, RunRecord, RunStore, StoreError};

const STAGE: &str = "ci";
/// One page of results; an answer that says there are more than it holds is unusable.
const PAGE: usize = 100;
/// A GitLab pipeline row carries urls and timestamps, so a smaller page keeps the whole
/// listing well inside the output cap of one provider call.
const GITLAB_PAGE: usize = 30;
/// Keeps only what the verdict reads; a full page of raw check runs would not fit the output cap.
const CHECK_RUN_FIELDS: &str =
    "{total_count, check_runs: [.check_runs[] | {name, status, conclusion}]}";
/// The `--jq` projection of the commit-status answer: only what the verdict reads,
/// so a long status list still fits the output cap.
const STATUS_FIELDS: &str = "{state, total_count, statuses: [.statuses[] | {state, context}]}";
/// Consecutive failed polls tolerated before the stage gives up on the provider.
const MAX_POLL_FAILURES: u32 = 3;
/// Floor on the pause between polls, so a zero interval cannot hammer the provider.
const MIN_POLL_INTERVAL: Duration = Duration::from_millis(50);
const MAX_FAILING_NAMES: usize = 20;
const MAX_NAME_CHARS: usize = 100;

#[derive(Debug, Clone)]
pub struct CiWaitConfig {
    /// A disabled stage does nothing and asks the provider nothing.
    pub enabled: bool,
    pub poll_interval: Duration,
    /// How long to keep waiting for checks that are missing or unfinished.
    pub timeout: Duration,
    /// How long one provider call may take.
    pub command_timeout: Duration,
    /// How long a repository with no CI configuration may report no checks before the stage
    /// is skipped: a workflow file just pushed can take a moment to register.
    pub no_ci_grace: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CiOutcome {
    /// The stage is disabled, or the repository has no CI configured.
    Skipped,
    /// Every check for the head SHA finished green.
    Passed,
    /// These checks (or the pipeline) finished red.
    Failed { failing: Vec<String> },
    /// Checks were still missing or unfinished when the timeout passed.
    TimedOut,
    /// The provider could not be asked or answered with something unusable.
    Errored { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Verdict {
    Passed,
    Failed(Vec<String>),
    Pending,
    /// The source reported nothing at all for the commit.
    Empty,
    Unusable(String),
}

#[derive(Clone, Copy)]
enum Kind {
    GitHub,
    GitLab,
}

/// Wait for the CI of `head_sha` on the run's repository with the provider CLI at `program`.
pub fn wait_for_ci(
    store: &RunStore,
    run_id: &str,
    program: &Path,
    head_sha: &str,
    config: &CiWaitConfig,
) -> Result<CiOutcome, StoreError> {
    wait_for_ci_logged(store, run_id, program, head_sha, config, None)
}

/// `wait_for_ci`, writing a line to `log` after each look at the checks, so someone watching can see it is waiting.
pub fn wait_for_ci_logged(
    store: &RunStore,
    run_id: &str,
    program: &Path,
    head_sha: &str,
    config: &CiWaitConfig,
    log: Option<&Path>,
) -> Result<CiOutcome, StoreError> {
    let record = store.load(run_id)?;
    if !config.enabled {
        record_event(
            store,
            &record,
            "stage.skipped",
            serde_json::json!({ "reason": "disabled" }),
        )?;
        return Ok(CiOutcome::Skipped);
    }
    let kind = match record.provider.as_str() {
        "github" => Kind::GitHub,
        "gitlab" => Kind::GitLab,
        other => {
            return Err(StoreError::Corrupt(format!(
                "the provider {other} has no CI to wait for"
            )))
        }
    };
    let repo = repository_url(&record)?;
    let Some(calls) = poll_calls(kind, &repo, head_sha) else {
        let reason = "the repository or commit cannot be addressed safely".to_string();
        let payload =
            serde_json::json!({ "result": "errored", "reason": reason, "head_sha": head_sha });
        return fail(store, &record, payload, CiOutcome::Errored { reason });
    };
    record_event(
        store,
        &record,
        "stage.started",
        serde_json::json!({ "provider": record.provider, "head_sha": head_sha }),
    )?;

    let sessions: Vec<DraftSession> = calls
        .into_iter()
        .map(|args| DraftSession {
            program: program.to_string_lossy().into_owned(),
            args,
            env: provider_environment(),
            cwd: record.worktree.clone(),
        })
        .collect();
    let deadline = Instant::now() + config.timeout;
    let mut failures = 0;
    loop {
        // A call never runs past the stage deadline, so nothing is accepted after it.
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            note(log, &format!("CI timed out waiting for the checks of {}", short(head_sha)));
            let payload = serde_json::json!({ "result": "timed_out", "head_sha": head_sha });
            return fail(store, &record, payload, CiOutcome::TimedOut);
        }
        let verdict = poll_once(&sessions, kind, head_sha, config, deadline);
        let waited = config.timeout.saturating_sub(deadline.saturating_duration_since(Instant::now()));
        match &verdict {
            Verdict::Pending => note(log, &format!("CI waiting: checks of {} still running ({}s of {}s)", short(head_sha), waited.as_secs(), config.timeout.as_secs())),
            Verdict::Empty => note(log, &format!("CI waiting: no checks reported for {} yet ({}s of {}s)", short(head_sha), waited.as_secs(), config.timeout.as_secs())),
            Verdict::Unusable(_) => note(log, &format!("CI waiting: the provider did not answer usably, trying again ({failures_so_far} so far)", failures_so_far = failures + 1)),
            _ => {}
        }
        if verdict == Verdict::Empty && waited >= config.no_ci_grace && !has_ci_config(&record.worktree, kind) {
            note(log, &format!("CI skipped: no CI is configured and no checks were reported for {}", short(head_sha)));
            record_event(
                store,
                &record,
                "stage.skipped",
                serde_json::json!({ "reason": "no CI configured", "head_sha": head_sha }),
            )?;
            return Ok(CiOutcome::Skipped);
        }
        match verdict {
            Verdict::Passed => {
                record_event(
                    store,
                    &record,
                    "stage.completed",
                    serde_json::json!({ "result": "passed", "head_sha": head_sha }),
                )?;
                return Ok(CiOutcome::Passed);
            }
            Verdict::Failed(failing) => {
                let payload = serde_json::json!({ "result": "failed", "failing": failing, "head_sha": head_sha });
                return fail(store, &record, payload, CiOutcome::Failed { failing });
            }
            Verdict::Unusable(reason) => {
                failures += 1;
                if failures >= MAX_POLL_FAILURES {
                    let payload = serde_json::json!({ "result": "errored", "reason": reason, "head_sha": head_sha });
                    return fail(store, &record, payload, CiOutcome::Errored { reason });
                }
            }
            Verdict::Pending | Verdict::Empty => failures = 0,
        }
        std::thread::sleep(
            config
                .poll_interval
                .max(MIN_POLL_INTERVAL)
                .min(deadline.saturating_duration_since(Instant::now())),
        );
    }
}

/// Whether the checked-out head defines any CI for the provider: a workflow file for GitHub,
/// the pipeline file for GitLab. A directory that cannot be read counts as configured, so an
/// unreadable one is waited on, never skipped.
fn has_ci_config(worktree: &Path, kind: Kind) -> bool {
    match kind {
        Kind::GitLab => worktree.join(".gitlab-ci.yml").exists(),
        Kind::GitHub => match std::fs::read_dir(worktree.join(".github/workflows")) {
            Ok(entries) => entries.filter_map(Result::ok).any(|entry| {
                matches!(entry.path().extension().and_then(|e| e.to_str()), Some("yml" | "yaml"))
            }),
            Err(err) => err.kind() != std::io::ErrorKind::NotFound,
        },
    }
}

fn short(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

/// A line for whoever is watching; a log that cannot be written never fails the wait.
fn note(log: Option<&Path>, text: &str) {
    use std::io::Write;
    if let Some(mut file) = log.and_then(|path| crate::implement::open_log(path).ok()) {
        let _ = writeln!(file, "{text}");
    }
}

/// One poll: every call is made in turn and the answers are combined. A red or unreadable
/// answer ends the poll early, since nothing the later calls say could change it.
fn poll_once(
    sessions: &[DraftSession],
    kind: Kind,
    head_sha: &str,
    config: &CiWaitConfig,
    deadline: Instant,
) -> Verdict {
    let mut verdicts = Vec::new();
    for (index, session) in sessions.iter().enumerate() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let verdict = match run_drafter(session, config.command_timeout.min(remaining)) {
            Ok(text) => match (kind, index) {
                (Kind::GitHub, 0) => github_verdict(&text),
                (Kind::GitHub, _) => github_status_verdict(&text),
                (Kind::GitLab, _) => gitlab_verdict(&text, head_sha),
            },
            Err(reason) => Verdict::Unusable(reason),
        };
        if matches!(verdict, Verdict::Failed(_) | Verdict::Unusable(_)) {
            return verdict;
        }
        verdicts.push(verdict);
    }
    if verdicts.contains(&Verdict::Pending) {
        Verdict::Pending
    } else if verdicts.contains(&Verdict::Passed) {
        Verdict::Passed
    } else if !verdicts.is_empty() && verdicts.iter().all(|v| *v == Verdict::Empty) {
        Verdict::Empty
    } else {
        Verdict::Pending
    }
}

/// The read-only API calls that list the checks of one commit. Everything that ends up in the
/// request path is checked first, since it comes from a remote url and a recorded sha.
fn poll_calls(kind: Kind, repo: &str, head_sha: &str) -> Option<Vec<Vec<String>>> {
    let sha_ok =
        (40..=64).contains(&head_sha.len()) && head_sha.chars().all(|c| c.is_ascii_hexdigit());
    let (_, rest) = repo.split_once("://")?;
    let (host, path) = rest.split_once('/')?;
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let safe = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | ':'))
    };
    if !sha_ok
        || !safe(host)
        || !safe(path)
        || path
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == "..")
    {
        return None;
    }
    let strings = |parts: &[&str]| parts.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    Some(match kind {
        Kind::GitHub => vec![
            strings(&[
                "api",
                "--hostname",
                host,
                &format!("repos/{path}/commits/{head_sha}/check-runs?per_page={PAGE}"),
                "--jq",
                CHECK_RUN_FIELDS,
            ]),
            // Older integrations report through commit statuses, which check runs do not include.
            strings(&[
                "api",
                "--hostname",
                host,
                &format!("repos/{path}/commits/{head_sha}/status"),
                "--jq",
                STATUS_FIELDS,
            ]),
        ],
        Kind::GitLab => {
            let project = path.replace('/', "%2F");
            vec![strings(&[
                "api",
                "--hostname",
                host,
                &format!("projects/{project}/pipelines?sha={head_sha}&per_page={GITLAB_PAGE}"),
            ])]
        }
    })
}

fn github_verdict(text: &str) -> Verdict {
    let unusable =
        |why: &str| Verdict::Unusable(format!("the check-run listing is unusable: {why}"));
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text.trim()) else {
        return unusable("not json");
    };
    let (Some(total), Some(runs)) = (
        value.get("total_count").and_then(|t| t.as_u64()),
        value.get("check_runs").and_then(|r| r.as_array()),
    ) else {
        return unusable("no check runs");
    };
    if total as usize > runs.len() {
        return unusable("more check runs than one page holds");
    }
    let (mut failing, mut pending) = (Vec::new(), false);
    for run in runs {
        let name = run.get("name").and_then(|n| n.as_str()).map(clean_name);
        let status = run.get("status").and_then(|s| s.as_str());
        let conclusion = run.get("conclusion").and_then(|c| c.as_str());
        match (status, conclusion) {
            (Some("queued" | "in_progress" | "waiting" | "pending" | "requested"), _) => {
                pending = true
            }
            (Some("completed"), Some("success" | "neutral" | "skipped")) => {}
            (
                Some("completed"),
                Some(
                    "failure" | "timed_out" | "cancelled" | "action_required" | "startup_failure"
                    | "stale",
                ),
            ) => {
                if failing.len() < MAX_FAILING_NAMES {
                    failing.push(name.unwrap_or_else(|| "unnamed check".to_string()));
                }
            }
            _ => return unusable("a check run has an unrecognised state"),
        }
    }
    if !failing.is_empty() {
        Verdict::Failed(failing)
    } else if pending {
        Verdict::Pending
    } else if runs.is_empty() {
        Verdict::Empty
    } else {
        Verdict::Passed
    }
}

/// The combined status of a commit: its own `state` sums up every status, so a list cut short
/// by paging cannot hide a red one.
fn github_status_verdict(text: &str) -> Verdict {
    let unusable = |why: &str| Verdict::Unusable(format!("the commit status is unusable: {why}"));
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text.trim()) else {
        return unusable("not json");
    };
    let (Some(total), Some(state), Some(statuses)) = (
        value.get("total_count").and_then(|t| t.as_u64()),
        value.get("state").and_then(|s| s.as_str()),
        value.get("statuses").and_then(|s| s.as_array()),
    ) else {
        return unusable("no state");
    };
    if total == 0 {
        return Verdict::Empty;
    }
    match state {
        "success" => Verdict::Passed,
        "pending" => Verdict::Pending,
        "failure" | "error" => {
            let mut failing: Vec<String> = statuses
                .iter()
                .filter(|s| {
                    matches!(
                        s.get("state").and_then(|s| s.as_str()),
                        Some("failure" | "error")
                    )
                })
                .filter_map(|s| s.get("context").and_then(|c| c.as_str()).map(clean_name))
                .take(MAX_FAILING_NAMES)
                .collect();
            if failing.is_empty() {
                failing.push("commit status".to_string());
            }
            Verdict::Failed(failing)
        }
        _ => unusable("an unrecognised state"),
    }
}

fn gitlab_verdict(text: &str, head_sha: &str) -> Verdict {
    let unusable =
        |why: &str| Verdict::Unusable(format!("the pipeline listing is unusable: {why}"));
    let Ok(serde_json::Value::Array(rows)) = serde_json::from_str::<serde_json::Value>(text.trim())
    else {
        return unusable("not a json array");
    };
    if rows.len() >= GITLAB_PAGE {
        return unusable("more pipelines than one page holds");
    }
    // The newest pipeline for exactly this commit is the one that counts.
    // Every row for it must be well formed: a row that cannot be read might be the newest.
    let mut matching = Vec::new();
    for row in rows
        .iter()
        .filter(|row| row.get("sha").and_then(|s| s.as_str()) == Some(head_sha))
    {
        let (Some(id), Some(status)) = (
            row.get("id").and_then(|i| i.as_u64()),
            row.get("status").and_then(|s| s.as_str()),
        ) else {
            return unusable("a pipeline for the commit has no readable id or status");
        };
        matching.push((id, status));
    }
    let latest = matching.into_iter().max_by_key(|(id, _)| *id);
    match latest {
        None => Verdict::Pending,
        Some((_, "success")) => Verdict::Passed,
        Some((id, "failed" | "canceled")) => Verdict::Failed(vec![format!("pipeline {id}")]),
        Some((
            _,
            "created" | "waiting_for_resource" | "preparing" | "pending" | "running" | "scheduled",
        )) => Verdict::Pending,
        Some(_) => unusable("a pipeline has an unrecognised state"),
    }
}

/// A check's name is the provider's text: stripped of control characters and bounded.
fn clean_name(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_control())
        .take(MAX_NAME_CHARS)
        .collect()
}

fn fail(
    store: &RunStore,
    record: &RunRecord,
    payload: serde_json::Value,
    outcome: CiOutcome,
) -> Result<CiOutcome, StoreError> {
    record_event(store, record, "stage.failed", payload)?;
    Ok(outcome)
}

fn record_event(
    store: &RunStore,
    record: &RunRecord,
    kind: &str,
    payload: serde_json::Value,
) -> Result<(), StoreError> {
    store.append_event(&Event {
        schema_version: 1,
        event_id: next_event_id(&record.run_id),
        run_id: record.run_id.clone(),
        project_id: record.project_id.clone(),
        feature_id: record.feature_id.clone(),
        kind: kind.to_string(),
        stage: Some(STAGE.to_string()),
        attempt: 1,
        occurred_at: now_rfc3339(),
        payload,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runs(rows: &str, total: usize) -> String {
        format!("{{\"total_count\":{total},\"check_runs\":[{rows}]}}")
    }
    fn run(name: &str, status: &str, conclusion: &str) -> String {
        let c = if conclusion.is_empty() {
            "null".to_string()
        } else {
            format!("\"{conclusion}\"")
        };
        format!("{{\"name\":\"{name}\",\"status\":\"{status}\",\"conclusion\":{c}}}")
    }

    #[test]
    fn github_all_completed_successfully_passes() {
        let text = runs(
            &[
                run("a", "completed", "success"),
                run("b", "completed", "skipped"),
            ]
            .join(","),
            2,
        );
        assert_eq!(github_verdict(&text), Verdict::Passed);
    }

    #[test]
    fn github_a_failed_run_fails_even_while_others_are_pending() {
        let text = runs(
            &[
                run("a", "in_progress", ""),
                run("b", "completed", "failure"),
            ]
            .join(","),
            2,
        );
        assert_eq!(
            github_verdict(&text),
            Verdict::Failed(vec!["b".to_string()])
        );
    }

    #[test]
    fn github_unfinished_or_absent_runs_keep_waiting() {
        assert_eq!(
            github_verdict(&runs(&run("a", "queued", ""), 1)),
            Verdict::Pending
        );
        assert_eq!(github_verdict(&runs("", 0)), Verdict::Empty);
    }

    #[test]
    fn github_an_unusable_answer_is_never_green() {
        assert!(matches!(github_verdict("not json"), Verdict::Unusable(_)));
        assert!(matches!(
            github_verdict(&runs(&run("a", "completed", "success"), 150)),
            Verdict::Unusable(_)
        ));
        assert!(matches!(
            github_verdict(&runs(&run("a", "completed", "mystery"), 1)),
            Verdict::Unusable(_)
        ));
        assert!(matches!(
            github_verdict(&runs(&run("a", "wobbling", ""), 1)),
            Verdict::Unusable(_)
        ));
    }

    const SHA: &str = "abc";

    fn pipelines(rows: &[(u64, &str, &str)]) -> String {
        let parts: Vec<String> = rows
            .iter()
            .map(|(id, sha, st)| format!("{{\"id\":{id},\"sha\":\"{sha}\",\"status\":\"{st}\"}}"))
            .collect();
        format!("[{}]", parts.join(","))
    }

    #[test]
    fn gitlab_the_latest_pipeline_for_the_sha_decides() {
        let text = pipelines(&[
            (5, SHA, "success"),
            (9, SHA, "failed"),
            (12, "other", "success"),
        ]);
        assert_eq!(
            gitlab_verdict(&text, SHA),
            Verdict::Failed(vec!["pipeline 9".to_string()])
        );
        assert_eq!(
            gitlab_verdict(&pipelines(&[(9, SHA, "success")]), SHA),
            Verdict::Passed
        );
    }

    #[test]
    fn gitlab_a_malformed_row_for_the_sha_is_unusable_not_ignored() {
        let text = format!("[{{\"id\":4,\"sha\":\"{SHA}\",\"status\":\"success\"}},{{\"id\":9,\"sha\":\"{SHA}\"}}]");
        assert!(matches!(gitlab_verdict(&text, SHA), Verdict::Unusable(_)));
        let text = format!("[{{\"id\":4,\"sha\":\"{SHA}\",\"status\":\"success\"}},{{\"id\":\"9\",\"sha\":\"{SHA}\",\"status\":\"failed\"}}]");
        assert!(matches!(gitlab_verdict(&text, SHA), Verdict::Unusable(_)));
    }

    #[test]
    fn gitlab_running_or_absent_pipelines_keep_waiting_and_odd_ones_are_unusable() {
        assert_eq!(
            gitlab_verdict(&pipelines(&[(9, SHA, "running")]), SHA),
            Verdict::Pending
        );
        assert_eq!(gitlab_verdict("[]", SHA), Verdict::Pending);
        assert!(matches!(
            gitlab_verdict(&pipelines(&[(9, SHA, "manual")]), SHA),
            Verdict::Unusable(_)
        ));
        assert!(matches!(gitlab_verdict("{}", SHA), Verdict::Unusable(_)));
    }
}
