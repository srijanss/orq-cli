//! The implement-and-review stage: run the implementer harness in the run's
//! worktree and record what happened as stage events.

use std::fs;
use std::io::{Read, Write};
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
    /// A file the session writes as its last step: once it exists the session is closed by orq and the
    /// stage counts as finished. A file already there when the session starts is removed first.
    pub exit_marker: Option<&'a Path>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImplementOutcome {
    pub success: bool,
    /// `None` when the program could not be started or was killed by a signal.
    pub exit_code: Option<i32>,
    /// What the session said went wrong, cleaned for the run's events, when it reported a problem.
    pub reported: Option<String>,
}

pub fn run_implement_stage(
    store: &RunStore,
    run_id: &str,
    session: &ImplementSession<'_>,
) -> Result<ImplementOutcome, StoreError> {
    run_session_stage(store, run_id, session, STAGE, 1)
}

/// Run one harness session as `stage`, recording `stage.started` and
/// `stage.completed` / `stage.failed` events numbered `attempt`.
pub(crate) fn run_session_stage(
    store: &RunStore,
    run_id: &str,
    session: &ImplementSession<'_>,
    stage: &str,
    attempt: u32,
) -> Result<ImplementOutcome, StoreError> {
    run_session_stage_with(
        store,
        run_id,
        session,
        stage,
        attempt,
        serde_json::json!({}),
    )
}

/// [`run_session_stage`], with the fields of `fields` (an object) on every event it records.
pub(crate) fn run_session_stage_with(
    store: &RunStore,
    run_id: &str,
    session: &ImplementSession<'_>,
    stage: &str,
    attempt: u32,
    fields: serde_json::Value,
) -> Result<ImplementOutcome, StoreError> {
    let record = store.load(run_id)?;
    let log = open_log(session.log)?;
    let event = |kind: &str, extra: serde_json::Value| {
        let mut payload = serde_json::json!({
            "session_id": session.session_id,
            "log": session.log.to_string_lossy(),
        });
        if let (Some(map), Some(more)) = (payload.as_object_mut(), fields.as_object()) {
            map.extend(more.clone());
        }
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
            stage: Some(stage.to_string()),
            attempt,
            occurred_at: now_rfc3339(),
            payload,
        }
    };
    let counted = log_line_count(&log);
    let started = match counted {
        Some((lines, bytes)) => serde_json::json!({ "log_line": lines, "log_offset": bytes }),
        None => serde_json::json!({}),
    };
    store.append_event(&event("stage.started", started))?;
    let began = std::time::Instant::now();
    let began_at = crate::parse_rfc3339(&now_rfc3339()).unwrap_or(0);
    // The transcript an earlier session's hooks named is not this session's.
    if session.terminal {
        let _ = store.write_run_file(run_id, crate::session_usage::TRANSCRIPT_FILE, b"");
    }

    let mut command = Command::new(session.program);
    command
        .args(session.args)
        .current_dir(session.cwd)
        .env_clear();
    for name in REQUIRED_ENV {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command.envs(session.env.iter().map(|(name, value)| (name, value)));
    // A session on the terminal writes nothing to its log, so orq writes there how it started and ended.
    let mut terminal_log = None;
    let mut progress_log = None;
    if session.terminal {
        command
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        terminal_log = Some(log);
    } else {
        // Stdout goes through orq, which writes each step of the session as a readable line.
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(log.try_clone()?);
        progress_log = Some(log);
    }

    // A marker left by an earlier session must not close this one; if it cannot be cleared the stage fails.
    let cleared = match session.exit_marker {
        Some(marker) => match fs::remove_file(marker) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err),
            _ => Ok(()),
        },
        None => Ok(()),
    };
    // A session runs in a process group of its own, so ending it ends its descendants too. On the
    // terminal it also takes the foreground, and orq takes it back afterwards.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
        if session.terminal {
            // SAFETY: only async-signal-safe calls (signal, isatty, tcsetpgrp, getpid) run between fork and exec.
            unsafe {
                command.pre_exec(|| {
                    if libc::isatty(0) == 1 {
                        // The new group is in the background until it takes the terminal; SIGTTOU is ignored meanwhile.
                        let old = libc::signal(libc::SIGTTOU, libc::SIG_IGN);
                        libc::tcsetpgrp(0, libc::getpid());
                        libc::signal(libc::SIGTTOU, old);
                    }
                    Ok(())
                });
            }
        }
    }
    // A session closed by orq never gets to undo what it did to the terminal, so orq does.
    #[cfg(unix)]
    let terminal_settings = session.terminal.then(saved_terminal_settings).flatten();
    let status = cleared
        .and_then(|()| crate::child_groups::spawn(&mut command))
        .and_then(|mut child| {
            let progress = match (child.take_output().0, progress_log.take()) {
                (Some(stdout), Some(log)) => Some(spawn_progress(stdout, log)),
                _ => None,
            };
            if let Some(log) = &terminal_log {
                log_line(
                    log,
                    &format!(
                        "started {} (pid {}) in {}",
                        session.program,
                        child.id(),
                        session.cwd.display()
                    ),
                );
            }
            let end = wait_for_session(child, session.exit_marker);
            // The rest of the output is written before the log is read; a descendant that escaped the
            // session's group and still holds its stdout does not hold up the stage.
            if let Some(progress) = progress {
                let _ = progress.recv_timeout(PROGRESS_DRAIN);
            }
            #[cfg(unix)]
            if let Some(settings) = &terminal_settings {
                // A session that exited by itself has put its screen away and left its last words
                // on it; one orq closed, or one a signal killed, still has its screen up.
                let screen_left_up =
                    !matches!(&end, Ok(SessionEnd::Exited(status)) if status.code().is_some());
                restore_terminal(settings, screen_left_up);
            }
            end
        });
    let marker = session.exit_marker.and_then(marker_report);
    let (reported, notes) = match marker {
        Some(Marker::Problem(problem)) => (Some(problem), None),
        Some(Marker::Finished { notes }) => (None, notes),
        None => (None, None),
    };
    // A session that failed by its exit status has already failed, whatever its marker says.
    let finished = match &status {
        Ok(SessionEnd::Closed) => true,
        Ok(SessionEnd::Exited(status)) => status.success(),
        Err(_) => false,
    };
    let signal = match &status {
        Ok(SessionEnd::Exited(status)) => exit_signal(status),
        _ => None,
    };
    let ended = match &status {
        Ok(SessionEnd::Closed) => {
            format!(
                "{} was closed by orq after it wrote its exit marker",
                session.program
            )
        }
        Ok(SessionEnd::Exited(status)) => match status.code() {
            Some(code) => format!("{} exited with status {code}", session.program),
            None => format!(
                "{} was stopped by {}",
                session.program,
                describe_signal(signal)
            ),
        },
        Err(err) => format!("could not start {}: {err}", session.program),
    };
    let (outcome, reason) = match status {
        _ if finished && reported.is_some() => (
            ImplementOutcome {
                success: false,
                exit_code: Some(0),
                reported: reported.clone(),
            },
            reported.map(|problem| format!("the session reported a problem: {problem}")),
        ),
        Ok(SessionEnd::Closed) => (
            ImplementOutcome {
                success: true,
                exit_code: Some(0),
                reported: None,
            },
            None,
        ),
        Ok(SessionEnd::Exited(status)) if status.success() => (
            ImplementOutcome {
                success: true,
                exit_code: status.code(),
                reported: None,
            },
            None,
        ),
        // How a session that failed ended is the reason it failed.
        Ok(SessionEnd::Exited(status)) => (
            ImplementOutcome {
                success: false,
                exit_code: status.code(),
                reported: None,
            },
            Some(ended.clone()),
        ),
        Err(_) => (
            ImplementOutcome {
                success: false,
                exit_code: None,
                reported: None,
            },
            Some(ended.clone()),
        ),
    };
    if let Some(log) = &terminal_log {
        log_line(log, &ended);
        // A reason the end line does not already give, such as a problem the session reported.
        if let Some(reason) = reason.as_ref().filter(|reason| **reason != ended) {
            log_line(log, reason);
        }
    }
    let closing = match reason {
        None => event(
            "stage.completed",
            serde_json::json!({ "exit_code": outcome.exit_code }),
        ),
        Some(reason) => {
            let mut payload =
                serde_json::json!({ "exit_code": outcome.exit_code, "reason": reason });
            if let Some(signal) = signal {
                payload["signal"] = serde_json::json!(signal);
            }
            event("stage.failed", payload)
        }
    };
    store.append_event(&closing)?;
    // What the session used, when the agent CLI said: in an unattended session's output (its log), or
    // in the transcript an attended session's hooks named.
    let usage = if session.terminal {
        store
            .read_run_file(run_id, crate::session_usage::TRANSCRIPT_FILE, 4096)
            .ok()
            .flatten()
            .and_then(|path| String::from_utf8(path).ok())
            .filter(|path| !path.is_empty())
            .and_then(|path| {
                crate::session_usage::usage_from_transcript(Path::new(&path), began_at)
            })
    } else {
        counted.and_then(|(_, offset)| crate::session_usage::usage_from_log(session.log, offset))
    };
    if let Some(usage) = usage {
        store.append_event(&event(
            "session.usage",
            usage.payload(began.elapsed().as_secs()),
        ))?;
    }
    // Notes from a session that finished are kept for the report; a failed stage already says why.
    if let Some(notes) = notes.filter(|_| outcome.success) {
        store.append_event(&event(
            "session.notes",
            serde_json::json!({ "notes": notes }),
        ))?;
    }
    Ok(outcome)
}

/// Writes one timestamped line of orq's own to a session log. The log is a record for people, so a
/// write that fails does not fail the stage.
fn log_line(mut log: &fs::File, text: &str) {
    let _ = writeln!(log, "[orq {}] {text}", now_rfc3339());
}

/// How long the stage waits, once the session has ended, for the rest of its output to be written.
const PROGRESS_DRAIN: std::time::Duration = std::time::Duration::from_secs(5);
/// The most of a summary that goes on a progress line.
const PROGRESS_SUMMARY_LIMIT: usize = 160;

/// Writes the session's stdout to `log` on a thread of its own, one readable line per step of a
/// stream-json session; the receiver hears once all of it is written.
fn spawn_progress(
    stdout: std::process::ChildStdout,
    log: fs::File,
) -> std::sync::mpsc::Receiver<()> {
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        write_progress(stdout, &log);
        let _ = done.send(());
    });
    finished
}

/// Reads `output` line by line and appends what each line says to `log`. A line that is not a
/// stream-json event is written as it is.
fn write_progress(output: impl Read, mut log: &fs::File) {
    use std::io::BufRead;
    let mut reader = std::io::BufReader::new(output);
    let mut tools = std::collections::HashMap::new();
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let text = String::from_utf8_lossy(&line);
        let written = match progress_event(text.trim_end_matches(['\n', '\r']), &mut tools) {
            Some(lines) => log.write_all(lines.as_bytes()),
            None => log.write_all(&line),
        };
        if written.is_err() {
            // The log is a record for people: the session goes on, and its output is still drained.
            let _ = std::io::copy(&mut reader, &mut std::io::sink());
            break;
        }
    }
}

/// The log lines for one stream-json event, or `None` when `line` is not one. `tools` maps each tool
/// call's id to its tool's name, so a result can be told apart by the tool it answers.
fn progress_event(
    line: &str,
    tools: &mut std::collections::HashMap<String, String>,
) -> Option<String> {
    let event: serde_json::Value = serde_json::from_str(line).ok()?;
    let kind = event.get("type")?.as_str()?;
    let content = || {
        event
            .pointer("/message/content")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
    };
    let clock = &now_rfc3339()[11..19];
    let mut out = String::new();
    match kind {
        "assistant" => {
            for item in content() {
                match item.get("type").and_then(serde_json::Value::as_str) {
                    Some("text") => {
                        let said = summary(item.get("text").and_then(serde_json::Value::as_str));
                        if !said.is_empty() {
                            out.push_str(&format!("{clock} said {said}\n"));
                        }
                    }
                    Some("tool_use") => {
                        let name = item
                            .get("name")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("tool");
                        if let Some(id) = item.get("id").and_then(serde_json::Value::as_str) {
                            tools.insert(id.to_string(), name.to_string());
                        }
                        let name = summary(Some(name));
                        let about = summary(item.get("input").and_then(tool_argument));
                        let line = format!("{clock} tool {name} {about}");
                        out.push_str(line.trim_end());
                        out.push('\n');
                    }
                    _ => {}
                }
            }
        }
        "user" => {
            for item in content() {
                if item.get("type").and_then(serde_json::Value::as_str) != Some("tool_result") {
                    continue;
                }
                let tool = item
                    .get("tool_use_id")
                    .and_then(serde_json::Value::as_str)
                    .and_then(|id| tools.remove(id));
                let failed =
                    item.get("is_error").and_then(serde_json::Value::as_bool) == Some(true);
                let text = result_text(item.get("content"));
                let tests = tool
                    .filter(|name| name == "run_tests" || name.ends_with("__run_tests"))
                    .and(test_counts(&text));
                let line = match tests {
                    Some((passed, failed_tests, millis)) => {
                        let state = if failed || failed_tests > 0 {
                            "error"
                        } else {
                            "ok"
                        };
                        let took = millis
                            .map(|ms| format!(" in {:.1}s", ms as f64 / 1000.0))
                            .unwrap_or_default();
                        format!("{state} passed {passed}, failed {failed_tests}{took}")
                    }
                    None if failed => format!("error {}", summary(Some(&text))),
                    None => continue,
                };
                out.push_str(&format!("{clock} result {}", line.trim_end()));
                out.push('\n');
            }
        }
        // The raw event first, so the session's usage can still be read from the log, then its final
        // answer as the log's last line, as a session in text mode prints it.
        "result" => {
            out.push_str(line);
            out.push('\n');
            if let Some(answer) = event.get("result").and_then(serde_json::Value::as_str) {
                out.push_str(answer);
                if !answer.ends_with('\n') {
                    out.push('\n');
                }
            }
        }
        "system" | "stream_event" | "rate_limit_event" => {}
        _ => return None,
    }
    Some(out)
}

/// What a tool call is about: its description (a command's, not the command itself), else the file
/// or search it names, else its first text argument.
fn tool_argument(input: &serde_json::Value) -> Option<&str> {
    const KEYS: [&str; 8] = [
        "description",
        "file_path",
        "notebook_path",
        "pattern",
        "path",
        "query",
        "url",
        "command",
    ];
    let input = input.as_object()?;
    KEYS.iter()
        .find_map(|key| input.get(*key).and_then(serde_json::Value::as_str))
        .or_else(|| input.values().find_map(serde_json::Value::as_str))
}

/// The text of a tool result, whether given as a string or as a list of text parts.
fn result_text(content: Option<&serde_json::Value>) -> String {
    match content {
        Some(serde_json::Value::String(text)) => text.clone(),
        Some(serde_json::Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| part.get("text").and_then(serde_json::Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// The passed and failed counts and the duration in milliseconds of a `run_tests` result.
fn test_counts(text: &str) -> Option<(u64, u64, Option<u64>)> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let result = value.get("testResult").unwrap_or(&value);
    let passed = result.get("passed")?.as_u64()?;
    let failed = result.get("failed")?.as_u64()?;
    Some((
        passed,
        failed,
        result.get("durationMs").and_then(serde_json::Value::as_u64),
    ))
}

/// The first non-blank line of `text`, redacted, with control characters as spaces and cut to
/// `PROGRESS_SUMMARY_LIMIT` characters.
fn summary(text: Option<&str>) -> String {
    let first = text
        .unwrap_or("")
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    // Only the line shown is redacted: a tool's whole output can be large.
    orq_core::redact::redact(first)
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(PROGRESS_SUMMARY_LIMIT)
        .collect()
}

/// The signal that stopped a session, when one did.
fn exit_signal(status: &std::process::ExitStatus) -> Option<i32> {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status.signal()
    }
    #[cfg(not(unix))]
    {
        let _ = status;
        None
    }
}

/// `signal 9 (SIGKILL) — often the kernel's out-of-memory killer; check dmesg`, or `a signal` when unknown.
fn describe_signal(signal: Option<i32>) -> String {
    let Some(number) = signal else {
        return "a signal".to_string();
    };
    match signal_name(number) {
        Some(name @ "SIGKILL") => {
            format!(
                "signal {number} ({name}) — often the kernel's out-of-memory killer; check dmesg"
            )
        }
        Some(name) => format!("signal {number} ({name})"),
        None => format!("signal {number}"),
    }
}

fn signal_name(number: i32) -> Option<&'static str> {
    #[cfg(unix)]
    {
        const NAMES: [(i32, &str); 15] = [
            (libc::SIGHUP, "SIGHUP"),
            (libc::SIGINT, "SIGINT"),
            (libc::SIGQUIT, "SIGQUIT"),
            (libc::SIGILL, "SIGILL"),
            (libc::SIGTRAP, "SIGTRAP"),
            (libc::SIGABRT, "SIGABRT"),
            (libc::SIGBUS, "SIGBUS"),
            (libc::SIGFPE, "SIGFPE"),
            (libc::SIGKILL, "SIGKILL"),
            (libc::SIGUSR1, "SIGUSR1"),
            (libc::SIGSEGV, "SIGSEGV"),
            (libc::SIGUSR2, "SIGUSR2"),
            (libc::SIGPIPE, "SIGPIPE"),
            (libc::SIGALRM, "SIGALRM"),
            (libc::SIGTERM, "SIGTERM"),
        ];
        NAMES
            .iter()
            .find(|(n, _)| *n == number)
            .map(|(_, name)| *name)
    }
    #[cfg(not(unix))]
    {
        let _ = number;
        None
    }
}

/// A marker is read only this far: it is a short note, and nothing limits what a session writes there.
const MARKER_READ_LIMIT: u64 = 4096;
/// The most of a report that goes into the run's events: room for a few paragraphs of what went wrong.
const MARKER_REPORT_LIMIT: usize = 2000;

/// What a session's finish marker says. The first line decides: `OK` (or nothing at all) is finished,
/// with anything after `OK` (on its line after a colon or a space, or on the lines after it) kept as
/// notes; `PROBLEM` is a failure with the text after it as the reason;
/// anything else is a failure with the whole text as the reason.
#[derive(Debug, PartialEq, Eq)]
enum Marker {
    Finished { notes: Option<String> },
    Problem(String),
}

impl Marker {
    #[cfg(test)]
    fn into_problem(self) -> Option<String> {
        match self {
            Marker::Problem(text) => Some(text),
            Marker::Finished { .. } => None,
        }
    }
}

/// What the session wrote into its marker, `None` when there is no marker.
fn marker_report(marker: &Path) -> Option<Marker> {
    marker_report_with(marker, || {})
}

/// `marker_report`, running `between` just before the path is opened, so a test can swap the path at
/// exactly that point.
fn marker_report_with(marker: &Path, between: impl FnOnce()) -> Option<Marker> {
    const NOT_PLAIN: &str = "its finish marker is not a plain file";
    let mut open = fs::OpenOptions::new();
    open.read(true);
    // Whatever the path is by now: a link is not followed, which would copy another file's contents
    // into the run's events, and a pipe cannot block orq.
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        open.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    between();
    let file = match open.open(marker) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
        #[cfg(unix)]
        Err(err) if err.raw_os_error() == Some(libc::ELOOP) => {
            return Some(Marker::Problem(NOT_PLAIN.to_string()))
        }
        Err(err) => {
            return Some(Marker::Problem(format!(
                "its finish marker could not be read: {err}"
            )))
        }
    };
    match file.metadata() {
        Ok(found) if found.is_file() => {}
        Ok(_) => return Some(Marker::Problem(NOT_PLAIN.to_string())),
        Err(err) => {
            return Some(Marker::Problem(format!(
                "its finish marker could not be read: {err}"
            )))
        }
    }
    let mut bytes = Vec::new();
    if let Err(err) = file.take(MARKER_READ_LIMIT).read_to_end(&mut bytes) {
        return Some(Marker::Problem(format!(
            "its finish marker could not be read: {err}"
        )));
    }
    let redacted = orq_core::redact::redact(&String::from_utf8_lossy(&bytes));
    let text = redacted.trim_start();
    if text.is_empty() {
        // Blanks up to the read limit say nothing about what comes after them, so that is not a finish.
        return Some(if bytes.len() as u64 >= MARKER_READ_LIMIT {
            Marker::Problem(
                "its finish marker holds more than orq reads, and the part it read is blank"
                    .to_string(),
            )
        } else {
            Marker::Finished { notes: None }
        });
    }
    let (first, after) = text.split_once('\n').unwrap_or((text, ""));
    let first = first.trim();
    // `OK` alone, or followed by notes on the same line (after a colon or a space) or the lines after it.
    let ok_rest = first
        .strip_prefix("OK")
        .filter(|rest| rest.is_empty() || rest.starts_with([' ', ':']));
    if let Some(rest) = ok_rest {
        // Read up to the limit with no line end seen, the line may go on past it ("OK tests are failing").
        if after.is_empty() && !text.contains('\n') && bytes.len() as u64 >= MARKER_READ_LIMIT {
            return Some(Marker::Problem(
                "its finish marker holds more than orq reads, and its first line is cut"
                    .to_string(),
            ));
        }
        let notes = clean(&format!("{} {after}", rest.trim_start_matches(':')));
        return Some(Marker::Finished {
            notes: (!notes.is_empty()).then_some(notes),
        });
    }
    // `PROBLEM` alone, or followed by the reason on the same line or the lines after it.
    let reason = first
        .strip_prefix("PROBLEM")
        .filter(|rest| rest.is_empty() || rest.starts_with([' ', ':']))
        .map(|rest| clean(&format!("{} {after}", rest.trim_start_matches(':'))));
    Some(Marker::Problem(match reason {
        Some(reason) if reason.is_empty() => {
            "it reported a problem without saying what".to_string()
        }
        Some(reason) => reason,
        None => clean(text),
    }))
}

/// `text` as one line of at most `MARKER_REPORT_LIMIT` characters, with control characters as spaces.
fn clean(text: &str) -> String {
    let spaced: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    spaced.trim().chars().take(MARKER_REPORT_LIMIT).collect()
}

enum SessionEnd {
    Exited(std::process::ExitStatus),
    /// The marker appeared and orq closed the session.
    Closed,
}

/// How long a session is given to end by itself once it has written its marker.
const MARKER_GRACE: std::time::Duration = std::time::Duration::from_millis(500);

/// Waits for `child`; with a marker, closes it once the marker exists and a short grace has passed.
fn wait_for_session(
    child: crate::child_groups::TrackedChild,
    marker: Option<&Path>,
) -> std::io::Result<SessionEnd> {
    let mut seen: Option<std::time::Instant> = None;
    loop {
        // Looking does not reap, so the leader's id stays reserved until the group has been signalled.
        match child.has_exited() {
            Ok(false) => {}
            Ok(true) => {
                end_group(child.id());
                return child.reap().map(SessionEnd::Exited);
            }
            // Dropping the child ends the session; the terminal has to come back first.
            Err(err) => {
                end_group(child.id());
                return Err(err);
            }
        }
        if marker.is_some_and(Path::exists) {
            let since = *seen.get_or_insert_with(std::time::Instant::now);
            if since.elapsed() >= MARKER_GRACE {
                end_group(child.id());
                let _ = child.reap();
                return Ok(SessionEnd::Closed);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Ends every process still in the session's own group, so none outlives the stage.
fn end_group(leader: u32) {
    #[cfg(unix)]
    {
        take_back_terminal();
        // SAFETY: the session leads a group of its own, so its pid names that group.
        unsafe { libc::killpg(leader as libc::pid_t, libc::SIGKILL) };
    }
    #[cfg(not(unix))]
    let _ = leader;
}

/// Makes orq's own process group the terminal's foreground group again, when stdin is a terminal.
#[cfg(unix)]
fn take_back_terminal() {
    // SAFETY: plain libc calls on orq's own process; SIGTTOU is ignored only for the duration of the call.
    unsafe {
        if libc::isatty(0) == 1 {
            let old = libc::signal(libc::SIGTTOU, libc::SIG_IGN);
            libc::tcsetpgrp(0, libc::getpgrp());
            libc::signal(libc::SIGTTOU, old);
        }
    }
}

/// The terminal's settings, when stdin is a terminal.
#[cfg(unix)]
fn saved_terminal_settings() -> Option<libc::termios> {
    // SAFETY: an all-zero termios is valid for tcgetattr to fill in.
    let mut settings: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: `settings` is valid for the call.
    (unsafe { libc::isatty(0) == 1 && libc::tcgetattr(0, &mut settings) == 0 }).then_some(settings)
}

/// Undoes the screen modes an agent's own screen turns on: its alternate screen, a hidden cursor,
/// bracketed paste, focus and mouse reports, and its keyboard mode. Then clears the screen, so what
/// orq prints next is not drawn over what the session left.
#[cfg(unix)]
const SCREEN_RESET: &str = "\x1b[?1049l\x1b[?25h\x1b[?2004l\x1b[?1004l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[<u\x1b[H\x1b[2J";

/// Puts back settings taken by `saved_terminal_settings`: a session killed in raw mode would
/// otherwise leave Enter unable to end a line. With `reset_screen`, also resets the screen, when
/// stdout is a terminal.
#[cfg(unix)]
fn restore_terminal(settings: &libc::termios, reset_screen: bool) {
    // SAFETY: plain libc calls; SIGTTOU is ignored only for the duration of the calls.
    unsafe {
        let old = libc::signal(libc::SIGTTOU, libc::SIG_IGN);
        libc::tcsetattr(0, libc::TCSANOW, settings);
        if reset_screen && libc::isatty(1) == 1 {
            use std::io::Write;
            let mut stdout = std::io::stdout().lock();
            let _ = stdout
                .write_all(SCREEN_RESET.as_bytes())
                .and_then(|()| stdout.flush());
        }
        libc::signal(libc::SIGTTOU, old);
    }
}

/// Open a stage log for appending. Output can hold source and credentials, so
/// the file is owner-only, and a symlink is never followed: a new file is
/// created exclusively, an existing one must be a regular file and the very
/// file that was inspected.
pub(crate) fn open_log(path: &Path) -> std::io::Result<fs::File> {
    open_log_with(path, || {})
}

/// `open_log`, running `between` after the path is inspected and before it is
/// opened, so a test can swap the path at exactly that point.
fn open_log_with(path: &Path, between: impl FnOnce()) -> std::io::Result<fs::File> {
    let refuse = |what: &str| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{} is {what}", path.display()),
        )
    };
    let mut open = fs::OpenOptions::new();
    // Read too, so the lines already in it can be counted through the very handle that was checked.
    open.append(true).read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // No symlink is followed, and a FIFO swapped in cannot block the open.
        open.mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    match fs::symlink_metadata(path) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            between();
            // `create_new` is O_EXCL: it fails rather than follow a link planted meanwhile.
            open.create_new(true).open(path)
        }
        Err(err) => Err(err),
        Ok(meta) if !meta.is_file() => Err(refuse("not a regular file")),
        #[cfg(unix)]
        Ok(meta) => {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            between();
            let file = open.open(path)?;
            let opened = file.metadata()?;
            if !opened.is_file() {
                return Err(refuse("not a regular file"));
            }
            if opened.dev() != meta.dev() || opened.ino() != meta.ino() {
                return Err(refuse("not the file that was inspected"));
            }
            // Tighten one from earlier runs through the handle, so a swapped path cannot redirect it.
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
            Ok(file)
        }
        // Without no-follow opening and handle identity checks an existing file cannot be trusted.
        #[cfg(not(unix))]
        Ok(_) => Err(refuse(
            "an existing file, which cannot be opened safely on this platform",
        )),
    }
}

/// How many lines and bytes the log open as `log` holds now, read from its start without moving its
/// offset; `None` when it cannot be read. A stage start records both, so the part of an appended log a
/// later attempt wrote can be told from an earlier one's and read from where it starts.
pub(crate) fn log_line_count(log: &fs::File) -> Option<(u64, u64)> {
    let mut buffer = [0u8; 64 * 1024];
    let mut at = 0u64;
    let mut lines = 0u64;
    loop {
        #[cfg(unix)]
        let read = std::os::unix::fs::FileExt::read_at(log, &mut buffer, at).ok()?;
        #[cfg(not(unix))]
        let read = std::os::windows::fs::FileExt::seek_read(log, &mut buffer, at).ok()?;
        if read == 0 {
            return Some((lines, at));
        }
        lines += buffer[..read].iter().filter(|&&byte| byte == b'\n').count() as u64;
        at += read as u64;
    }
}

pub(crate) fn next_event_id(run_id: &str) -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!(
        "{run_id}-{nanos}-{}",
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

/// The current UTC time as `YYYY-MM-DDTHH:MM:SSZ`.
pub(crate) fn now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs()) as i64;
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
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3_600,
        rem % 3_600 / 60,
        rem % 60
    )
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::process::Command;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn a_log_swapped_for_a_fifo_after_inspection_is_refused_without_blocking() {
        let dir = orq_test_support::process_temp_dir()
            .join(format!("orq-open-log-fifo-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let log = dir.join("stage.log");
        fs::write(&log, "old\n").unwrap();
        let (sender, receiver) = mpsc::channel();
        let path = log.clone();
        std::thread::spawn(move || {
            let swap = || {
                fs::remove_file(&path).unwrap();
                assert!(Command::new("mkfifo")
                    .arg(&path)
                    .status()
                    .unwrap()
                    .success());
            };
            let _ = sender.send(open_log_with(&path, swap).is_err());
        });

        let refused = receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("open_log blocked on the FIFO");

        assert!(refused);
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = orq_test_support::process_temp_dir()
            .join(format!("orq-open-log-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_log_swapped_for_a_symlink_after_inspection_is_refused_and_the_target_untouched() {
        let dir = scratch("link");
        let log = dir.join("stage.log");
        let victim = dir.join("victim");
        fs::write(&log, "old\n").unwrap();
        fs::write(&victim, "precious\n").unwrap();

        let result = open_log_with(&log, || {
            fs::remove_file(&log).unwrap();
            std::os::unix::fs::symlink(&victim, &log).unwrap();
        });

        assert!(result.is_err());
        assert_eq!(fs::read_to_string(&victim).unwrap(), "precious\n");
    }

    #[test]
    fn a_log_swapped_for_another_regular_file_after_inspection_is_refused() {
        let dir = scratch("other");
        let log = dir.join("stage.log");
        let other = dir.join("other");
        fs::write(&log, "old\n").unwrap();
        fs::write(&other, "other\n").unwrap();

        let result = open_log_with(&log, || {
            // Renaming over the log keeps both inodes alive, so none can be reused.
            fs::rename(&other, &log).unwrap();
        });

        assert!(result.is_err(), "a different inode than the inspected one");
    }

    #[test]
    fn a_log_planted_before_exclusive_creation_is_not_adopted() {
        let dir = scratch("planted");
        let log = dir.join("stage.log");
        let victim = dir.join("victim");
        fs::write(&victim, "precious\n").unwrap();

        let result = open_log_with(&log, || std::os::unix::fs::symlink(&victim, &log).unwrap());

        assert!(result.is_err());
        assert_eq!(fs::read_to_string(&victim).unwrap(), "precious\n");
    }

    #[test]
    fn a_marker_swapped_after_it_was_inspected_is_not_followed_and_does_not_block() {
        let dir = orq_test_support::process_temp_dir()
            .join(format!("orq-marker-swap-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let (marker, secret) = (dir.join("done"), dir.join("secret"));
        fs::write(&secret, "the contents of another file").unwrap();

        fs::write(&marker, "").unwrap();
        let report = marker_report_with(&marker, || {
            fs::remove_file(&marker).unwrap();
            std::os::unix::fs::symlink(&secret, &marker).unwrap();
        })
        .and_then(Marker::into_problem)
        .expect("a link is a problem to report");
        assert!(!report.contains("another file"), "{report}");

        let fifo = dir.join("pipe");
        fs::write(&fifo, "").unwrap();
        let (sender, answer) = std::sync::mpsc::channel();
        let path = fifo.clone();
        std::thread::spawn(move || {
            let report = marker_report_with(&path, || {
                fs::remove_file(&path).unwrap();
                let name = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
                // Safety: plain system call on a path this test owns.
                assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
            });
            let _ = sender.send(report);
        });
        let report = answer
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the read blocked on a pipe");
        assert!(
            report.and_then(Marker::into_problem).is_some(),
            "a pipe is a problem to report"
        );
    }
}
