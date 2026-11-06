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
    /// How long the newest log has been silent, when there is one.
    pub quiet_for: Option<Duration>,
}

pub fn run_activity(events: &[Event], log: Option<&LogTail>, stuck_after: Duration) -> RunActivity {
    let summary = summarize_run(events);
    let running = matches!(summary.phase, RunPhase::Running | RunPhase::PauseRequested);
    RunActivity {
        phase: summary.phase,
        stage: summary.stage,
        last_output: log.and_then(|l| l.last_line.as_deref()).map(orq_core::redact::redact),
        quiet_for: log.map(|l| l.quiet_for),
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
    let (_, path, _) = newest?;
    // Opened without following a link and without waiting on a pipe, then judged by the handle
    // itself: the path may have been swapped since it was listed.
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path)
        .ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() {
        return None;
    }
    let (modified, size) = (meta.modified().ok()?, meta.len());
    file.seek(SeekFrom::Start(size.saturating_sub(TAIL_BYTES))).ok()?;
    let mut bytes = Vec::new();
    file.take(TAIL_BYTES).read_to_end(&mut bytes).ok()?;
    let start = size.saturating_sub(TAIL_BYTES);
    let text = String::from_utf8_lossy(&bytes);
    // A tail that begins mid-file starts mid-line: that cut line is dropped, since a secret
    // cut at its front would no longer be recognised.
    let text = if start > 0 { text.split_once('\n').map_or("", |(_, rest)| rest) } else { &text };
    // Redacted whole, then cut for display: the other way round could leave a secret's tail.
    let last_line = text
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| orq_core::redact::redact(line).chars().take(MAX_LINE_CHARS).collect());
    Some(LogTail { size, quiet_for: modified.elapsed().unwrap_or_default(), last_line })
}

/// The line to show for `current`, or none: a changed stage or last line of output is shown as it
/// comes, and a run that has shown nothing for `heartbeat` says it is still going.
pub fn activity_line(previous: Option<&RunActivity>, current: &RunActivity, since_last_line: Duration, heartbeat: Duration) -> Option<String> {
    let stage = current.stage.as_deref().unwrap_or("run");
    let changed = match previous {
        None => true,
        Some(p) => p.stage != current.stage || (current.last_output.is_some() && p.last_output != current.last_output),
    };
    if changed {
        return Some(match &current.last_output {
            Some(out) => format!("orq: {stage}: {out}"),
            None => format!("orq: {stage}"),
        });
    }
    if since_last_line >= heartbeat {
        let quiet = current.quiet_for.map_or(String::new(), |q| format!(", no output for {}s", q.as_secs()));
        return Some(format!("orq: {stage} still running{quiet}"));
    }
    None
}
