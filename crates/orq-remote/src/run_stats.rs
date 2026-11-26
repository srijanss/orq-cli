//! The Stats page's figures: how the runs went, computed from their events, never stored. What one
//! refresh learns from each run's events is a [`RunFacts`]; every window and project is then counted
//! from those, so a refresh reads each run's events once and no logs.

use std::collections::{BTreeMap, HashMap};

use orq_orchestrator::{Event, RunPhase};
use serde_json::{json, Value};

/// How a run ended, as the Stats page counts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Delivered,
    Failed,
    /// Continued as another run, which is counted on its own.
    Resumed,
    /// Running, paused or waiting.
    Open,
    /// Discarded: its events are gone, so it counts only as started and discarded.
    Discarded,
}

/// What the Stats page needs of one run.
#[derive(Debug, Clone)]
pub struct RunFacts {
    pub run_id: String,
    /// The project as the web names it.
    pub project: String,
    /// When its first event happened, in seconds since the epoch.
    pub started: u64,
    pub outcome: Outcome,
    /// The stage a failed run failed in.
    pub failed_stage: Option<String>,
    /// How long each stage it ran took, in seconds, in the order they ended.
    pub stage_times: Vec<(String, u64)>,
    pub gate_fixes: u64,
    /// The run it continued, when it was resumed from one.
    pub resumed_from: Option<String>,
    /// Whether it stopped to wait on a review escalation at some point.
    pub needed_review: bool,
    /// When its review was ready for a person.
    pub review_ready: Option<u64>,
    /// When that review was resolved, dismissed or merged.
    pub review_done: Option<u64>,
    /// The usage of each session it recorded; empty when none was, which is no data, not zero.
    pub usage: Vec<Usage>,
}

/// The tokens one agent session used, as its `session.usage` event recorded them.
#[derive(Debug, Clone, Default)]
pub struct Usage {
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

impl Usage {
    fn of(event: &Event) -> Usage {
        let count = |key: &str| event.payload.get(key).and_then(Value::as_u64).unwrap_or(0);
        Usage {
            model: event
                .payload
                .get("model")
                .and_then(Value::as_str)
                .filter(|model| !model.is_empty())
                .unwrap_or("unknown")
                .to_string(),
            input_tokens: count("input_tokens"),
            output_tokens: count("output_tokens"),
            cache_read_tokens: count("cache_read_tokens"),
            cache_write_tokens: count("cache_write_tokens"),
        }
    }

    fn add(&mut self, other: &Usage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cache_read_tokens += other.cache_read_tokens;
        self.cache_write_tokens += other.cache_write_tokens;
    }
}

fn secs(event: &Event) -> Option<u64> {
    orq_orchestrator::parse_rfc3339(&event.occurred_at)
}

/// What `events` (one run's, oldest first) say for the Stats page; `None` for a run with no readable start.
pub fn facts(run_id: &str, project: &str, events: &[Event]) -> Option<RunFacts> {
    let started = events.first().and_then(secs)?;
    let summary = orq_orchestrator::summarize_run(events);
    let outcome = match summary.phase {
        RunPhase::Completed if summary.resumed_as.is_some() => Outcome::Resumed,
        RunPhase::Completed => Outcome::Delivered,
        RunPhase::Failed | RunPhase::Blocked => Outcome::Failed,
        _ => Outcome::Open,
    };
    let failed_stage = (outcome == Outcome::Failed).then(|| {
        events
            .iter()
            .rev()
            .find(|event| event.kind == "stage.failed")
            .and_then(|event| event.stage.clone())
            .or_else(|| summary.stage.clone())
            .unwrap_or_else(|| "unknown".to_string())
    });
    let mut open: HashMap<(String, u32), u64> = HashMap::new();
    let mut stage_times = Vec::new();
    let mut gate_fixes = 0;
    let mut resumed_from = None;
    let mut needed_review = false;
    let mut review_ready = None;
    let mut review_done = None;
    for event in events {
        let ending = event.payload.get("ending").and_then(Value::as_str);
        let ready = matches!(event.kind.as_str(), "review.created" | "review.adopted")
            || (event.kind == "run.completed" && ending == Some("review_ready"));
        if ready && review_ready.is_none() {
            review_ready = secs(event);
        }
        if matches!(event.kind.as_str(), "review.resolved" | "review.dismissed")
            && review_ready.is_some()
            && review_done.is_none()
        {
            review_done = secs(event);
        }
        match (event.kind.as_str(), &event.stage) {
            ("stage.started", Some(stage)) => {
                if stage == "gate_fix" {
                    gate_fixes += 1;
                }
                if let Some(at) = secs(event) {
                    open.insert((stage.clone(), event.attempt), at);
                }
            }
            ("stage.completed" | "stage.failed", Some(stage)) => {
                if let (Some(begun), Some(at)) =
                    (open.remove(&(stage.clone(), event.attempt)), secs(event))
                {
                    stage_times.push((stage.clone(), at.saturating_sub(begun)));
                }
            }
            _ => {}
        }
        if resumed_from.is_none() {
            let key = match event.kind.as_str() {
                "run.sessions_carried" => Some("from"),
                "stage.completed" => Some("resumed_from"),
                _ => None,
            };
            resumed_from = key.and_then(|key| event.payload.get(key)?.as_str().map(str::to_string));
        }
        if event.kind == "run.completed" && ending == Some("needs_review") {
            needed_review = true;
        }
    }
    let usage = events
        .iter()
        .filter(|event| event.kind == "session.usage")
        .map(Usage::of)
        .collect();
    Some(RunFacts {
        run_id: run_id.to_string(),
        project: project.to_string(),
        started,
        outcome,
        failed_stage,
        stage_times,
        gate_fixes,
        resumed_from,
        needed_review,
        review_ready,
        review_done,
        usage,
    })
}

/// What is left of a discarded run of `project`: when it started.
pub fn discarded(run: &orq_orchestrator::DiscardedRun, project: &str) -> Option<RunFacts> {
    Some(RunFacts {
        run_id: run.run_id.clone(),
        project: project.to_string(),
        started: orq_orchestrator::parse_rfc3339(&run.started_at)?,
        outcome: Outcome::Discarded,
        failed_stage: None,
        stage_times: Vec::new(),
        gate_fixes: 0,
        resumed_from: None,
        needed_review: false,
        review_ready: None,
        review_done: None,
        usage: Vec::new(),
    })
}

/// `text` as one path segment: anything but letters, digits and `-._~` is percent-encoded.
fn encode(text: &str) -> String {
    text.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

/// The value at percentile `p` (0..=1) of `sorted`, by nearest rank.
fn percentile(sorted: &[u64], p: f64) -> u64 {
    let rank = ((p * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len());
    sorted[rank - 1]
}

/// The figures for `project` (every project when `None`) over the `days` days before `now`, from
/// every run's facts.
pub fn compute(all: &[RunFacts], project: Option<&str>, days: u64, now: u64) -> Value {
    let since = now.saturating_sub(days * 86_400);
    let started: Vec<&RunFacts> = all
        .iter()
        .filter(|run| run.started >= since && project.is_none_or(|project| run.project == project))
        .collect();
    let discarded: Vec<&str> = started
        .iter()
        .filter(|run| run.outcome == Outcome::Discarded)
        .map(|run| run.run_id.as_str())
        .collect();
    // Every other figure is of the runs whose events are still there.
    let runs: Vec<&RunFacts> = started
        .iter()
        .copied()
        .filter(|run| run.outcome != Outcome::Discarded)
        .collect();
    let by_id: HashMap<&str, &RunFacts> =
        all.iter().map(|run| (run.run_id.as_str(), run)).collect();
    let link = |ids: &[&str]| {
        let list = ids.join(",");
        match project {
            Some(project) => format!("/projects/{}/orchestrations?runs={list}", encode(project)),
            None => format!("/orchestrations?runs={list}"),
        }
    };
    let figure = |ids: Vec<&str>| json!({ "count": ids.len(), "runs": ids, "link": link(&ids) });
    let ids = |keep: &dyn Fn(&RunFacts) -> bool| {
        runs.iter()
            .filter(|run| keep(run))
            .map(|run| run.run_id.as_str())
            .collect::<Vec<_>>()
    };
    let review_again = |run: &RunFacts| {
        run.resumed_from
            .as_deref()
            .and_then(|from| by_id.get(from))
            .is_some_and(|from| from.needed_review)
    };

    let delivered = ids(&|run| run.outcome == Outcome::Delivered);
    let first_try =
        ids(&|run| run.outcome == Outcome::Delivered && run.gate_fixes == 0 && !review_again(run));
    let mut first_try_figure = figure(first_try);
    first_try_figure["of"] = json!(delivered.len());

    let mut failures: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for run in &runs {
        if let Some(stage) = &run.failed_stage {
            failures.entry(stage).or_default().push(&run.run_id);
        }
    }
    let failures_by_stage: Vec<Value> = failures
        .into_iter()
        .map(|(stage, ids)| {
            let mut row = figure(ids);
            row["stage"] = json!(stage);
            row
        })
        .collect();

    let mut times: BTreeMap<&str, (Vec<u64>, Vec<&str>)> = BTreeMap::new();
    for run in &runs {
        for (stage, took) in &run.stage_times {
            let (durations, ids) = times.entry(stage).or_default();
            durations.push(*took);
            if !ids.contains(&run.run_id.as_str()) {
                ids.push(&run.run_id);
            }
        }
    }
    let stage_times: Vec<Value> = times
        .into_iter()
        .map(|(stage, (mut durations, ids))| {
            durations.sort_unstable();
            let mut row = figure(ids);
            row["stage"] = json!(stage);
            row["count"] = json!(durations.len());
            row["median_secs"] = json!(percentile(&durations, 0.5));
            row["p90_secs"] = json!(percentile(&durations, 0.9));
            row
        })
        .collect();

    let mut attempts: BTreeMap<u64, Vec<&str>> = BTreeMap::new();
    for run in &runs {
        attempts
            .entry(run.gate_fixes)
            .or_default()
            .push(&run.run_id);
    }
    let gate_fix_attempts: Vec<Value> = attempts
        .into_iter()
        .map(|(n, ids)| {
            let mut row = figure(ids);
            row["attempts"] = json!(n);
            row
        })
        .collect();

    let mut waits = Vec::new();
    let mut waited = Vec::new();
    for run in &runs {
        if let (Some(ready), Some(done)) = (run.review_ready, run.review_done) {
            waits.push(done.saturating_sub(ready));
            waited.push(run.run_id.as_str());
        }
    }
    waits.sort_unstable();
    let mut review_wait = figure(waited);
    if !waits.is_empty() {
        review_wait["median_secs"] = json!(percentile(&waits, 0.5));
        review_wait["p90_secs"] = json!(percentile(&waits, 0.9));
    }

    let mut by_project: BTreeMap<&str, (Usage, Vec<&str>)> = BTreeMap::new();
    let mut by_model: BTreeMap<&str, (Usage, Vec<&str>)> = BTreeMap::new();
    for run in &runs {
        for used in &run.usage {
            for (key, totals) in [
                (run.project.as_str(), &mut by_project),
                (used.model.as_str(), &mut by_model),
            ] {
                let (sum, ids) = totals.entry(key).or_default();
                sum.add(used);
                if !ids.contains(&run.run_id.as_str()) {
                    ids.push(&run.run_id);
                }
            }
        }
    }
    let token_rows = |totals: BTreeMap<&str, (Usage, Vec<&str>)>, name: &str| {
        totals
            .into_iter()
            .map(|(key, (sum, ids))| {
                let mut row = figure(ids);
                row[name] = json!(key);
                row["input_tokens"] = json!(sum.input_tokens);
                row["output_tokens"] = json!(sum.output_tokens);
                row["cache_read_tokens"] = json!(sum.cache_read_tokens);
                row["cache_write_tokens"] = json!(sum.cache_write_tokens);
                row
            })
            .collect::<Vec<_>>()
    };

    json!({
        "tokens": {
            "by_project": token_rows(by_project, "project"),
            "by_model": token_rows(by_model, "model"),
            "no_data": figure(ids(&|run| run.usage.is_empty())),
        },
        "review_wait": review_wait,
        "review_waiting": figure(ids(&|run| run.review_ready.is_some() && run.review_done.is_none())),
        "days": days,
        "project": project,
        "outcomes": {
            "started": figure(started.iter().map(|run| run.run_id.as_str()).collect()),
            "delivered": figure(delivered),
            "failed": figure(ids(&|run| run.outcome == Outcome::Failed)),
            "discarded": figure(discarded),
        },
        "first_try": first_try_figure,
        "failures_by_stage": failures_by_stage,
        "stage_times": stage_times,
        "gate_fix_attempts": gate_fix_attempts,
    })
}
