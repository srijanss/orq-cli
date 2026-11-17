//! What a run is doing right now, in a few words a person can read: the stage, the last line
//! of output and whether it looks stuck. Pure over the events and a look at the newest log.

use std::borrow::Cow;
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
    // Between stages nothing runs: the finished stage is not repeated, and the silence of its old
    // log is not a sign of anything stuck.
    let between_stages = events
        .iter()
        .rev()
        .find(|e| matches!(e.kind.as_str(), "stage.started" | "stage.completed" | "stage.skipped"))
        .is_some_and(|e| e.kind != "stage.started");
    RunActivity {
        phase: summary.phase,
        stage: summary.stage.filter(|_| !(running && between_stages)),
        attempt: events.iter().rev().find(|e| e.stage.is_some()).map_or(0, |e| e.attempt),
        tests: log.and_then(|l| l.tests.clone()),
        last_output: log.and_then(|l| l.last_line.as_deref()).map(orq_core::redact::redact),
        quiet_for: log.map(|l| l.quiet_for),
        may_be_stuck: running && !between_stages && log.is_some_and(|l| l.quiet_for > stuck_after),
    }
}

/// How far a test run has come, read from `cargo test`, pytest or vitest output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestProgress {
    pub done: u64,
    pub total: u64,
    /// The name of the test that finished last.
    pub current: Option<String>,
}

/// What a line of the log starts with when it says how many tests the whole suite has.
const TEST_TOTAL_PREFIX: &str = "orq: tests to run: ";

/// The line a gate writes to its log before a test run: the number of tests across every test
/// binary, or that it is not known.
pub fn test_total_line(total: Option<u64>) -> String {
    match total {
        Some(total) => format!("{TEST_TOTAL_PREFIX}{total}"),
        None => format!("{TEST_TOTAL_PREFIX}unknown"),
    }
}

/// The progress of the latest test run in `text`: the total comes from its [`test_total_line`] and
/// the count from every result line after it, whichever test binary printed it, so it only goes up.
/// A run with no known total has none: one binary's `running N tests` says nothing about the suite.
/// pytest needs no total line: its own collection line (or xdist's workers line) starts a count,
/// read from its verbose, xdist or quiet result lines. `pytest -q` prints no collection line: its
/// quiet rows' own percentage is then the progress, as hundredths. vitest prints no total: after a
/// listed one, each finished file adds its tests and the closing `Tests ... (N)` line completes it.
pub fn test_progress(text: &str) -> Option<TestProgress> {
    let mut progress: Option<TestProgress> = None;
    // Whether the count was started by a listed total: vitest prints none of its own, so its lines
    // say nothing about a count pytest started.
    let mut listed = false;
    for line in text.lines() {
        if let Some(total) = line.strip_prefix(TEST_TOTAL_PREFIX) {
            progress = total.parse::<u64>().ok().and_then(starting_count);
            listed = true;
            continue;
        }
        // pytest colours its lines with `--color=yes` or `FORCE_COLOR`: they are read without it.
        let plain = if line.contains('\u{1b}') { Cow::Owned(printable(line)) } else { Cow::Borrowed(line) };
        if let Some(total) = pytest_total(&plain) {
            progress = starting_count(total);
            listed = false;
            continue;
        }
        if let Some(percent) = pytest_quiet_percent(&plain) {
            // `pytest -q` prints no collection line: its own percentage is then counted in hundredths.
            let p = progress.get_or_insert_with(|| {
                listed = false;
                TestProgress { done: 0, total: 100, current: None }
            });
            p.done = (p.total * percent / 100).min(p.total);
            p.current = None;
            continue;
        }
        let Some(p) = progress.as_mut() else { continue };
        if let Some(name) = pytest_result(&plain) {
            p.done = (p.done + 1).min(p.total);
            p.current = Some(name.to_string());
            continue;
        }
        if listed {
            if let Some((file, count)) = vitest_file(&plain) {
                p.done = p.done.saturating_add(count).min(p.total);
                p.current = Some(file.to_string());
                continue;
            }
            if vitest_summary(&plain) {
                p.done = p.total;
                continue;
            }
        }
        let Some(result) = line.trim().strip_prefix("test ") else { continue };
        let Some((name, outcome)) = result.rsplit_once(" ... ") else { continue };
        if matches!(outcome, "ok" | "FAILED" | "ignored") || outcome.starts_with("ignored,") {
            p.done = (p.done + 1).min(p.total);
            p.current = Some(name.to_string());
        }
    }
    progress
}

/// A count of none done out of `total`, or none at all when there is nothing to measure.
fn starting_count(total: u64) -> Option<TestProgress> {
    (total > 0).then_some(TestProgress { done: 0, total, current: None })
}

/// The number of tests a pytest collection line says will run: `collected N items`, or the
/// selected count when some were deselected (`collected N items / D deselected / S selected`).
/// With xdist (`-n`) the controller collects nothing itself and says `N workers [M items]`.
fn pytest_total(line: &str) -> Option<u64> {
    let line = line.trim();
    if let Some(total) = xdist_total(line) {
        return Some(total);
    }
    let rest = line.strip_prefix("collecting ... ").unwrap_or(line).strip_prefix("collected ")?;
    let mut parts = rest.split(" / ");
    let (count, unit) = parts.next()?.split_once(' ')?;
    if !matches!(unit, "items" | "item") {
        return None;
    }
    let collected = count.parse::<u64>().ok()?;
    let selected = parts.find_map(|part| part.strip_suffix(" selected")?.parse::<u64>().ok());
    Some(selected.unwrap_or(collected))
}

/// The item count of xdist's `N workers [M items]` (or `1 worker [1 item]`) line.
fn xdist_total(line: &str) -> Option<u64> {
    let (workers, items) = line.strip_suffix(']')?.split_once(" [")?;
    let (count, unit) = workers.split_once(' ')?;
    count.parse::<u32>().ok().filter(|_| matches!(unit, "workers" | "worker"))?;
    let (total, unit) = items.split_once(' ')?;
    total.parse::<u64>().ok().filter(|_| matches!(unit, "items" | "item"))
}

/// What pytest prints for a finished test.
const PYTEST_OUTCOMES: [&str; 6] = ["PASSED", "FAILED", "ERROR", "SKIPPED", "XFAIL", "XPASS"];

/// The test id of a verbose pytest result line: `<test id> <OUTCOME>[ (<reason>)] [ nn%]`, or with xdist
/// (`-n`) `[gwN] [ nn%] <OUTCOME> <test id>`.
fn pytest_result(line: &str) -> Option<&str> {
    let line = line.trim();
    if let Some(rest) = line.strip_prefix("[gw") {
        let (worker, rest) = rest.split_once("] [")?;
        worker.parse::<u32>().ok()?;
        let (percent, rest) = rest.split_once("%] ")?;
        pytest_percent(percent)?;
        let (outcome, name) = rest.split_once(' ')?;
        return finished_test(name, outcome);
    }
    let (result, _) = before_progress_column(line)?;
    let (name, outcome) = result.rsplit_once(' ')?;
    if let Some(name) = finished_test(name, outcome) {
        return Some(name);
    }
    // A skipped or expected failure says why after its outcome: `<test id> SKIPPED (<reason>)`.
    result.strip_suffix(')')?;
    result.match_indices(" (").find_map(|(at, _)| {
        let (name, outcome) = result[..at].rsplit_once(' ')?;
        finished_test(name, outcome)
    })
}

/// The percentage at the end of a row of quiet pytest output: `....F.s  [ 60%]`.
fn pytest_quiet_percent(line: &str) -> Option<u64> {
    let (marks, percent) = before_progress_column(line.trim())?;
    let quiet = !marks.is_empty() && marks.chars().all(|c| matches!(c, '.' | 'F' | 'E' | 's' | 'x' | 'X'));
    quiet.then_some(percent)
}

/// What a pytest line holds before its progress column `[ nn%]`, and that percentage.
fn before_progress_column(line: &str) -> Option<(&str, u64)> {
    let (before, column) = line.rsplit_once('[')?;
    Some((before.trim_end(), pytest_percent(column.strip_suffix("%]")?)?))
}

/// The number in pytest's progress column, `[ nn%]`, without its brackets and sign.
fn pytest_percent(text: &str) -> Option<u64> {
    text.trim().parse::<u64>().ok().filter(|percent| *percent <= 100)
}

/// `name`, when `outcome` is one pytest prints for a finished test.
fn finished_test<'a>(name: &'a str, outcome: &str) -> Option<&'a str> {
    let name = name.trim();
    PYTEST_OUTCOMES.contains(&outcome).then_some(name).filter(|name| !name.is_empty())
}

/// The file and test count of what vitest's default reporter prints as a test file finishes:
/// `<✓|❯|×|↓> <file> (<n> test[s][ | ...])[ <duration>...]`.
fn vitest_file(line: &str) -> Option<(&str, u64)> {
    let rest = line.trim().strip_prefix(['✓', '❯', '×', '↓'])?.strip_prefix(' ')?;
    let (file, rest) = rest.rsplit_once(" (")?;
    let (counts, after) = rest.split_once(')')?;
    let tests = counts.split(" | ").next()?;
    let count = tests.strip_suffix(" tests").or_else(|| tests.strip_suffix(" test"))?.parse::<u64>().ok()?;
    let after = after.trim();
    let file = file.trim();
    (!file.is_empty() && (after.is_empty() || after.split(' ').next().is_some_and(is_duration))).then_some((file, count))
}

/// Whether `line` is vitest's closing count of tests, `Tests  2 failed | 14 passed (16)`: the run
/// is over, so every listed test is done.
fn vitest_summary(line: &str) -> bool {
    let summary = || {
        let rest = line.trim().strip_prefix("Tests ")?;
        let (counts, total) = rest.strip_suffix(')')?.rsplit_once(" (")?;
        total.parse::<u64>().ok()?;
        let counted = |part: &str| part.split_once(' ').is_some_and(|(n, word)| n.parse::<u64>().is_ok() && !word.is_empty());
        counts.trim().split(" | ").all(counted).then_some(())
    };
    summary().is_some()
}

/// Whether `word` is a duration as vitest prints one: `34ms`, `1.20s`.
fn is_duration(word: &str) -> bool {
    word.strip_suffix("ms")
        .or_else(|| word.strip_suffix('s'))
        .is_some_and(|digits| !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit() || c == '.'))
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
