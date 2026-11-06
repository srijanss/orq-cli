//! What retention needs to know about every run, read from the run store and the
//! review database. A run whose facts cannot be read has none, and so is kept.

use std::collections::BTreeMap;

use orq_core::retention::RunFacts;
use orq_core::run::RunState;
use orq_core::storage::check_run_id;
use orq_core::store::Store;

use crate::run_status::{summarize_run, RunPhase};
use crate::store::{RunStore, StoreError};

pub fn collect_run_facts(
    runs: &RunStore,
    store: &Store,
) -> Result<BTreeMap<String, RunFacts>, StoreError> {
    let pinned: std::collections::BTreeSet<String> = store
        .pinned()
        .map_err(StoreError::from)?
        .into_iter()
        .collect();
    let mut all = BTreeMap::new();
    for run_id in runs.run_ids()? {
        if let Some(facts) = facts_of(runs, store, &run_id, pinned.contains(&run_id))? {
            all.insert(run_id, facts);
        }
    }
    Ok(all)
}

/// The facts of one run, read without touching any other run. `None` when the id is unsafe or the run cannot be read.
pub fn collect_run_facts_for(
    runs: &RunStore,
    store: &Store,
    run_id: &str,
) -> Result<Option<RunFacts>, StoreError> {
    if check_run_id(run_id).is_err() {
        return Ok(None);
    }
    let pinned = store.is_pinned(run_id).map_err(StoreError::from)?;
    facts_of(runs, store, run_id, pinned)
}

fn facts_of(
    runs: &RunStore,
    store: &Store,
    run_id: &str,
    pinned: bool,
) -> Result<Option<RunFacts>, StoreError> {
    if check_run_id(run_id).is_err() {
        return Ok(None);
    }
    let (Ok(record), Ok(events)) = (runs.load(run_id), runs.events(run_id)) else {
        return Ok(None);
    };
    let state = match summarize_run(&events).phase {
        RunPhase::Running => RunState::Running,
        RunPhase::Blocked => RunState::Blocked,
        RunPhase::Failed => RunState::Failed,
        RunPhase::Completed => RunState::Completed,
        RunPhase::PauseRequested => RunState::PauseRequested,
        RunPhase::Paused => RunState::Paused,
    };
    let ended_at = events
        .last()
        .and_then(|event| parse_rfc3339(&event.occurred_at));
    // Only a definite "not found" proves the worktree is gone; any other error leaves it unknown, so it counts as in use.
    let worktree_in_use = !matches!(std::fs::symlink_metadata(&record.worktree), Err(e) if e.kind() == std::io::ErrorKind::NotFound);
    // An unreadable review table is the same as not knowing: the run is kept either way.
    let review = store.review_observation(run_id).map_err(StoreError::from)?;
    Ok(Some(RunFacts {
        pinned,
        run_id: run_id.to_string(),
        state,
        ended_at,
        review,
        worktree_in_use,
    }))
}

/// Unix seconds of a `YYYY-MM-DDTHH:MM:SSZ` timestamp, the form events are written in.
pub(crate) fn parse_rfc3339(text: &str) -> Option<u64> {
    let bytes = text.as_bytes();
    if bytes.len() != 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || bytes[19] != b'Z'
    {
        return None;
    }
    let num = |range: std::ops::Range<usize>| {
        text.get(range)
            .filter(|s| s.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|s| s.parse::<i64>().ok())
    };
    let (year, month, day) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hour, minute, second) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    // Days since 1970-01-01 (Hinnant's algorithm, the inverse of `now_rfc3339`).
    let y = year - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + hour * 3_600 + minute * 60 + second).ok()
}
