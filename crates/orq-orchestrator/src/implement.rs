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
    /// A file the session writes as its last step: once it exists the session is closed by orq and the
    /// stage counts as finished. A file already there when the session starts is removed first.
    pub exit_marker: Option<&'a Path>,
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
    let record = store.load(run_id)?;
    let log = open_log(session.log)?;
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
            stage: Some(stage.to_string()),
            attempt,
            occurred_at: now_rfc3339(),
            payload,
        }
    };
    store.append_event(&event("stage.started", serde_json::json!({})))?;

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
    if session.terminal {
        command
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
    } else {
        command
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log);
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
    let status = cleared
        .and_then(|()| command.spawn())
        .and_then(|child| wait_for_session(child, session.exit_marker));
    let (outcome, reason) = match status {
        Ok(SessionEnd::Closed) => (
            ImplementOutcome {
                success: true,
                exit_code: Some(0),
            },
            None,
        ),
        Ok(SessionEnd::Exited(status)) if status.success() => (
            ImplementOutcome {
                success: true,
                exit_code: status.code(),
            },
            None,
        ),
        Ok(SessionEnd::Exited(status)) => {
            let reason = match status.code() {
                Some(code) => format!("{} exited with status {code}", session.program),
                None => format!("{} was stopped by a signal", session.program),
            };
            (
                ImplementOutcome {
                    success: false,
                    exit_code: status.code(),
                },
                Some(reason),
            )
        }
        Err(err) => (
            ImplementOutcome {
                success: false,
                exit_code: None,
            },
            Some(format!("could not start {}: {err}", session.program)),
        ),
    };
    let closing = match reason {
        None => event(
            "stage.completed",
            serde_json::json!({ "exit_code": outcome.exit_code }),
        ),
        Some(reason) => event(
            "stage.failed",
            serde_json::json!({ "exit_code": outcome.exit_code, "reason": reason }),
        ),
    };
    store.append_event(&closing)?;
    Ok(outcome)
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
    mut child: std::process::Child,
    marker: Option<&Path>,
) -> std::io::Result<SessionEnd> {
    let tracked = crate::child_groups::track(child.id());
    let mut seen: Option<std::time::Instant> = None;
    loop {
        // Looking does not reap, so the leader's id stays reserved until the group has been signalled.
        if crate::terminal_session::has_exited(&child)? {
            end_group(child.id());
            drop(tracked);
            return child.wait().map(SessionEnd::Exited);
        }
        if marker.is_some_and(Path::exists) {
            let since = *seen.get_or_insert_with(std::time::Instant::now);
            if since.elapsed() >= MARKER_GRACE {
                end_group(child.id());
                drop(tracked);
                let _ = child.kill();
                let _ = child.wait();
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
    open.append(true);
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
        let dir = std::env::temp_dir().join(format!("orq-open-log-fifo-{}", std::process::id()));
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
        let dir = std::env::temp_dir().join(format!("orq-open-log-{tag}-{}", std::process::id()));
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
}
