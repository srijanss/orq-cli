//! The implement-and-review stage: run the implementer harness in the run's
//! worktree and record what happened as stage events.

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::store::{Event, RunStore, StoreError};

const STAGE: &str = "implement_and_review";
/// What a harness needs to find its tools and its own configuration; the
/// rest of the caller's environment (tokens, CI variables) is not passed on.
const REQUIRED_ENV: [&str; 3] = ["PATH", "HOME", "TERM"];

/// One implementer session to run.
pub struct ImplementSession<'a> {
    pub program: &'a str,
    pub args: &'a [String],
    /// Extra variables beyond the required ones.
    pub env: &'a [(String, String)],
    pub cwd: &'a Path,
    pub session_id: Option<&'a str>,
    pub log: &'a Path,
    /// Attach to the invoking terminal instead of capturing output in the log.
    pub terminal: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImplementOutcome {
    pub success: bool,
    /// `None` when the program could not be started or was killed by a signal.
    pub exit_code: Option<i32>,
}

pub fn run_implement_stage(
    store: &RunStore,
    run_id: &str,
    session: &ImplementSession<'_>,
) -> Result<ImplementOutcome, StoreError> {
    let record = store.load(run_id)?;
    let log = fs::OpenOptions::new().create(true).append(true).open(session.log)?;
    let event = |kind: &str, extra: serde_json::Value| {
        let mut payload = serde_json::json!({
            "session_id": session.session_id,
            "log": session.log.to_string_lossy(),
        });
        if let (Some(map), Some(more)) = (payload.as_object_mut(), extra.as_object()) {
            map.extend(more.clone());
        }
        Event {
            schema_version: 1,
            event_id: next_event_id(run_id),
            run_id: record.run_id.clone(),
            project_id: record.project_id.clone(),
            feature_id: record.feature_id.clone(),
            kind: kind.to_string(),
            stage: Some(STAGE.to_string()),
            attempt: 1,
            occurred_at: now_rfc3339(),
            payload,
        }
    };
    store.append_event(&event("stage.started", serde_json::json!({})))?;

    let mut command = Command::new(session.program);
    command.args(session.args).current_dir(session.cwd).env_clear();
    for name in REQUIRED_ENV {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command.envs(session.env.iter().map(|(name, value)| (name, value)));
    if session.terminal {
        command.stdin(Stdio::inherit()).stdout(Stdio::inherit()).stderr(Stdio::inherit());
    } else {
        command.stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log);
    }

    let (outcome, reason) = match command.status() {
        Ok(status) if status.success() => (ImplementOutcome { success: true, exit_code: status.code() }, None),
        Ok(status) => {
            let reason = match status.code() {
                Some(code) => format!("{} exited with status {code}", session.program),
                None => format!("{} was stopped by a signal", session.program),
            };
            (ImplementOutcome { success: false, exit_code: status.code() }, Some(reason))
        }
        Err(err) => (
            ImplementOutcome { success: false, exit_code: None },
            Some(format!("could not start {}: {err}", session.program)),
        ),
    };
    let closing = match reason {
        None => event("stage.completed", serde_json::json!({ "exit_code": outcome.exit_code })),
        Some(reason) => event("stage.failed", serde_json::json!({ "exit_code": outcome.exit_code, "reason": reason })),
    };
    store.append_event(&closing)?;
    Ok(outcome)
}

fn next_event_id(run_id: &str) -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    format!("{run_id}-{nanos}-{}", NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
}

/// The current UTC time as `YYYY-MM-DDTHH:MM:SSZ`.
fn now_rfc3339() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from days since 1970-01-01 (Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rem / 3_600, rem % 3_600 / 60, rem % 60)
}
