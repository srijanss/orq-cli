//! What a run is doing right now, in a few words a person can read: the stage, the last line
//! of output and whether it looks stuck. Pure over the events and a look at the newest log.

use std::time::Duration;

use crate::run_status::{summarize_run, RunPhase};
use crate::store::Event;

/// A look at the newest log of a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogTail {
    pub size: u64,
    /// How long since the log was last written.
    pub quiet_for: Duration,
    pub last_line: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunActivity {
    pub phase: RunPhase,
    pub stage: Option<String>,
    pub last_output: Option<String>,
    pub may_be_stuck: bool,
}

pub fn run_activity(events: &[Event], log: Option<&LogTail>, stuck_after: Duration) -> RunActivity {
    let summary = summarize_run(events);
    let running = matches!(summary.phase, RunPhase::Running | RunPhase::PauseRequested);
    RunActivity {
        phase: summary.phase,
        stage: summary.stage,
        last_output: log.and_then(|l| l.last_line.as_deref()).map(orq_core::redact::redact),
        may_be_stuck: running && log.is_some_and(|l| l.quiet_for > stuck_after),
    }
}

/// How much of the end of a log is looked at, and the longest line shown.
const TAIL_BYTES: u64 = 8 * 1024;
const MAX_LINE_CHARS: usize = 200;

/// The most recently written `*.log` under `dir` (a run's log folder or one stage's), if any.
pub fn newest_log_tail(dir: &std::path::Path) -> Option<LogTail> {
    use std::io::{Read, Seek, SeekFrom};
    let mut newest: Option<(std::time::SystemTime, std::path::PathBuf, u64)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("log") {
            continue;
        }
        // Links are not followed: a log is a plain file of the run's own.
        let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
        let Ok(modified) = meta.modified() else { continue };
        if meta.is_file() && newest.as_ref().is_none_or(|(at, ..)| modified > *at) {
            newest = Some((modified, path, meta.len()));
        }
    }
    let (modified, path, size) = newest?;
    let mut file = std::fs::File::open(&path).ok()?;
    file.seek(SeekFrom::Start(size.saturating_sub(TAIL_BYTES))).ok()?;
    let mut bytes = Vec::new();
    file.take(TAIL_BYTES).read_to_end(&mut bytes).ok()?;
    let last_line = String::from_utf8_lossy(&bytes)
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| line.chars().take(MAX_LINE_CHARS).collect());
    Some(LogTail { size, quiet_for: modified.elapsed().unwrap_or_default(), last_line })
}
