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
    /// How far the test run in this log has come, when it holds one.
    pub tests: Option<TestProgress>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunActivity {
    pub phase: RunPhase,
    pub stage: Option<String>,
    /// The round (attempt) of the latest stage event; 0 before any stage has started.
    pub attempt: u32,
    pub last_output: Option<String>,
    pub tests: Option<TestProgress>,
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
        attempt: events.iter().rev().find(|e| e.stage.is_some()).map_or(0, |e| e.attempt),
        tests: log.and_then(|l| l.tests.clone()),
        last_output: log.and_then(|l| l.last_line.as_deref()).map(orq_core::redact::redact),
        quiet_for: log.map(|l| l.quiet_for),
        may_be_stuck: running && log.is_some_and(|l| l.quiet_for > stuck_after),
    }
}

/// How far a test run has come, read from `cargo test` output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestProgress {
    pub done: u64,
    pub total: u64,
    /// The name of the test that finished last.
    pub current: Option<String>,
}

/// The progress of the latest test run in `text`: the total comes from its `running N tests` line
/// and the count from the result lines after it. A run that has not begun, or has no tests, has none.
pub fn test_progress(text: &str) -> Option<TestProgress> {
    let mut progress: Option<TestProgress> = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(count) = line.strip_prefix("running ").and_then(|r| r.strip_suffix(" tests").or_else(|| r.strip_suffix(" test"))) {
            if let Ok(total) = count.trim().parse::<u64>() {
                progress = (total > 0).then_some(TestProgress { done: 0, total, current: None });
            }
            continue;
        }
        let Some(p) = progress.as_mut() else { continue };
        let Some(result) = line.strip_prefix("test ") else { continue };
        let Some((name, outcome)) = result.rsplit_once(" ... ") else { continue };
        if matches!(outcome, "ok" | "FAILED" | "ignored") || outcome.starts_with("ignored,") {
            p.done = (p.done + 1).min(p.total);
            p.current = Some(name.to_string());
        }
    }
    progress
}

/// How much of the end of a log is looked at, and the longest line shown.
const TAIL_BYTES: u64 = 8 * 1024;
const MAX_LINE_CHARS: usize = 200;
/// How much of the end of a log is read to count its tests.
const TEST_SCAN_BYTES: u64 = 4 * 1024 * 1024;

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
    file.by_ref().take(TAIL_BYTES).read_to_end(&mut bytes).ok()?;
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
    // The count needs more than the tail: the end of the log, up to a larger bound.
    let scan_from = size.saturating_sub(TEST_SCAN_BYTES);
    let mut scanned = Vec::new();
    let tests = file
        .seek(SeekFrom::Start(scan_from))
        .and_then(|_| file.by_ref().take(TEST_SCAN_BYTES).read_to_end(&mut scanned))
        .ok()
        .and_then(|_| {
            let text = String::from_utf8_lossy(&scanned);
            let text = if scan_from > 0 { text.split_once('\n').map_or("", |(_, rest)| rest) } else { &text };
            test_progress(text)
        })
        .map(|mut t| {
            t.current = t.current.map(|name| orq_core::redact::redact(&name).chars().take(MAX_LINE_CHARS).collect());
            t
        });
    Some(LogTail { size, quiet_for: modified.elapsed().unwrap_or_default(), last_line, tests })
}

/// The line to show for `current`, or none: a changed stage or last line of output is shown as it
/// comes, and a run that has shown nothing for `heartbeat` says it is still going.
pub fn activity_line(previous: Option<&RunActivity>, current: &RunActivity, since_last_line: Duration, heartbeat: Duration) -> Option<String> {
    let stage = printable(current.stage.as_deref().unwrap_or("run"));
    let changed = match previous {
        None => true,
        Some(p) => p.stage != current.stage || (current.last_output.is_some() && p.last_output != current.last_output),
    };
    if changed {
        return Some(match &current.last_output {
            Some(out) => format!("orq: {stage}: {}", printable(out)),
            None => format!("orq: {stage}"),
        });
    }
    if since_last_line >= heartbeat {
        let quiet = current.quiet_for.map_or(String::new(), |q| format!(", no output for {}s", q.as_secs()));
        return Some(format!("orq: {stage} still running{quiet}"));
    }
    None
}

/// The gate and CI as a named step with its round and, for tests, how far they have come: the
/// lines to keep up to date in place of the log lines. Other stages have none.
pub fn step_lines(current: &RunActivity) -> Option<Vec<String>> {
    let round = current.attempt.max(1);
    match current.stage.as_deref()? {
        "verify" => Some(match &current.tests {
            Some(t) => {
                let mut lines = vec![format!("Tests (Round {round}) - {}%", t.done * 100 / t.total.max(1))];
                if let Some(name) = t.current.as_deref().map(printable).filter(|n| !n.is_empty()) {
                    lines.push(format!("  - {name}"));
                }
                lines
            }
            None => vec![format!("Checks (Round {round})")],
        }),
        "ci" if current.last_output.as_deref().is_some_and(|o| o.contains("no checks reported")) => Some(vec!["CI pending".to_string()]),
        "ci" => Some(vec![format!("CI running (Round {round})")]),
        _ => None,
    }
}

/// Text safe to write to a terminal: escape sequences (colour, cursor, title, clipboard, links) and
/// every other control character are removed, so output can never redraw or rewrite the screen.
fn printable(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.next() {
                // CSI: parameters, then one final byte in @..~.
                Some('[') => {
                    for n in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&n) {
                            break;
                        }
                    }
                }
                // OSC, DCS, and the like: until BEL or the string terminator ESC \.
                Some(']' | 'P' | '^' | '_' | 'X') => {
                    while let Some(n) = chars.next() {
                        if n == '\u{7}' || (n == '\u{1b}' && chars.next_if_eq(&'\\').is_some()) {
                            break;
                        }
                    }
                }
                _ => {}
            },
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}
