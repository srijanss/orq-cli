//! Short activity lines on the terminal of a run orq started interactively, for the stages that
//! follow the agent session: without them the pane looks frozen while the gate, push and CI run.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use orq_orchestrator::{activity_line, newest_log_tail, run_activity, step_lines, RunActivity, RunStore};

const POLL: Duration = Duration::from_millis(500);
/// A run that shows nothing new is looked at less and less often, up to this.
const POLL_QUIET_MAX: Duration = Duration::from_secs(5);
const HEARTBEAT: Duration = Duration::from_secs(60);
const STUCK_AFTER: Duration = Duration::from_secs(300);

/// Prints activity lines until dropped. Printing never fails the run. Stopping is best-effort for a
/// line already being written: it is never waited for, so one that was in flight can still appear.
pub struct Progress {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Progress {
    pub fn start(state_root: PathBuf, run_id: String, logs: PathBuf) -> Self {
        let in_place = std::io::stderr().is_terminal();
        Self::start_rendering(state_root, run_id, logs, HEARTBEAT, Box::new(std::io::stderr()), in_place)
    }

    #[cfg(test)]
    pub fn start_with(state_root: PathBuf, run_id: String, logs: PathBuf, heartbeat: Duration, out: Box<dyn Write + Send>) -> Self {
        Self::start_rendering(state_root, run_id, logs, heartbeat, out, false)
    }

    /// With `in_place` the gate and CI are shown as step lines redrawn where they are; without it
    /// every change is a plain appended line.
    pub fn start_rendering(state_root: PathBuf, run_id: String, logs: PathBuf, heartbeat: Duration, mut out: Box<dyn Write + Send>, in_place: bool) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::spawn(move || {
            let store = RunStore::new(&state_root);
            let mut previous: Option<RunActivity> = None;
            let mut last_line = Instant::now();
            let mut wait = POLL;
            let mut drawn: Vec<String> = Vec::new();
            while !flag.load(Ordering::SeqCst) {
                if let Some(current) = look(&store, &run_id, &logs) {
                    if session_has_terminal(&current) {
                        drawn.clear();
                    } else if let Some(mut lines) = step_lines(&current).filter(|_| in_place) {
                        if current.may_be_stuck {
                            lines[0].push_str(" (it may be stuck)");
                        }
                        if lines != drawn && !flag.load(Ordering::SeqCst) {
                            let mut frame = String::new();
                            if !drawn.is_empty() {
                                frame.push_str(&format!("\x1b[{}A", drawn.len()));
                            }
                            for line in &lines {
                                frame.push_str(&format!("\r\x1b[2K{line}\n"));
                            }
                            // A shorter step leaves no stale line below it.
                            if lines.len() < drawn.len() {
                                frame.push_str("\x1b[J");
                            }
                            let _ = out.write_all(frame.as_bytes());
                            let _ = out.flush();
                            last_line = Instant::now();
                            drawn = lines;
                        }
                        wait = next_wait(previous.as_ref(), &current, wait);
                        previous = Some(current);
                        let slept = Instant::now();
                        while slept.elapsed() < wait && !flag.load(Ordering::SeqCst) {
                            std::thread::sleep(Duration::from_millis(50));
                        }
                        continue;
                    }
                    drawn.clear();
                    // Checked again just before writing: a printer that was dropped while it looked says nothing.
                    if let Some(line) = activity_line(previous.as_ref(), &current, last_line.elapsed(), heartbeat).filter(|_| !session_has_terminal(&current) && !flag.load(Ordering::SeqCst)) {
                        let stuck = if current.may_be_stuck { " (it may be stuck)" } else { "" };
                        let _ = writeln!(out, "{line}{stuck}");
                        let _ = out.flush();
                        last_line = Instant::now();
                    }
                    wait = next_wait(previous.as_ref(), &current, wait);
                    previous = Some(current);
                }
                let slept = Instant::now();
                while slept.elapsed() < wait && !flag.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        });
        Self { stop, thread: Some(thread) }
    }
}

/// A fix session draws on the terminal itself, so a line printed over it would garble its screen.
fn session_has_terminal(current: &RunActivity) -> bool {
    matches!(current.stage.as_deref(), Some("gate_fix" | "ci_fix"))
}

/// How long to wait before looking again: back to the short wait when something new was shown,
/// else twice as long, up to the quiet maximum. Time passing in a silent log is not news.
fn next_wait(previous: Option<&RunActivity>, current: &RunActivity, wait: Duration) -> Duration {
    let news = previous.is_none_or(|p| p.phase != current.phase || p.stage != current.stage || p.last_output != current.last_output || p.tests != current.tests || p.may_be_stuck != current.may_be_stuck);
    if news { POLL } else { (wait * 2).min(POLL_QUIET_MAX) }
}

fn look(store: &RunStore, run_id: &str, logs: &Path) -> Option<RunActivity> {
    let events = store.events(run_id).ok()?;
    let tail = newest_log_tail(logs);
    Some(run_activity(&events, tail.as_ref(), STUCK_AFTER))
}

impl Drop for Progress {
    fn drop(&mut self) {
        // Never joined: the printer may be stuck writing to a terminal that stopped taking output,
        // and the end of the run must not wait for it. It stops by itself, or with the process.
        self.stop.store(true, Ordering::SeqCst);
        self.thread.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct Sink(Arc<Mutex<Vec<u8>>>);
    impl Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn the_stage_and_its_output_are_printed_while_the_run_goes_on() {
        let root = orq_test_support::process_temp_dir().join(format!("orq-progress-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let store = RunStore::new(&root);
        let record = orq_orchestrator::RunRecord {
            run_id: "run-1".into(),
            project_id: "repo-1".into(),
            feature_id: "COM-01".into(),
            remote: "origin".into(),
            provider: "github".into(),
            target_branch: "main".into(),
            base_sha: "abc".into(),
            branch: "orq/com-01".into(),
            worktree: root.join("wt"),
            repository: String::new(),
            profiles: Default::default(),
        };
        store.start_run(&record).unwrap();
        store
            .append_event(&orq_orchestrator::Event {
                schema_version: 1,
                event_id: "e1".into(),
                run_id: "run-1".into(),
                project_id: "repo-1".into(),
                feature_id: "COM-01".into(),
                kind: "stage.started".into(),
                stage: Some("gate".into()),
                attempt: 1,
                occurred_at: "2026-11-02T10:00:00Z".into(),
                payload: serde_json::json!({}),
            })
            .unwrap();
        let logs = root.join("logs");
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(logs.join("gate.log"), "running 12 tests\n").unwrap();
        let written = Arc::new(Mutex::new(Vec::new()));

        let progress = Progress::start_with(root.clone(), "run-1".into(), logs, Duration::from_secs(60), Box::new(Sink(written.clone())));
        let until = Instant::now() + Duration::from_secs(10);
        while written.lock().unwrap().is_empty() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(50));
        }
        drop(progress);

        let text = String::from_utf8(written.lock().unwrap().clone()).unwrap();
        assert!(text.contains("gate") && text.contains("running 12 tests"), "{text}");
    }

    fn gate_run(tag: &str, log: &str) -> (PathBuf, PathBuf) {
        let (root, logs) = running_run(tag);
        RunStore::new(&root)
            .append_event(&orq_orchestrator::Event {
                schema_version: 1,
                event_id: "e1".into(),
                run_id: "run-1".into(),
                project_id: "repo-1".into(),
                feature_id: "COM-01".into(),
                kind: "stage.started".into(),
                stage: Some("verify".into()),
                attempt: 1,
                occurred_at: "2026-11-02T10:00:00Z".into(),
                payload: serde_json::json!({}),
            })
            .unwrap();
        std::fs::write(logs.join("gate.log"), log).unwrap();
        (root, logs)
    }

    #[test]
    fn on_a_terminal_the_step_is_redrawn_in_place_as_its_percentage_moves() {
        let (root, logs) = gate_run("inplace", "orq: tests to run: 2\nrunning 2 tests\ntest a::one ... ok\n");
        let written = Arc::new(Mutex::new(Vec::new()));
        let progress = Progress::start_rendering(root, "run-1".into(), logs.clone(), Duration::from_secs(60), Box::new(Sink(written.clone())), true);
        let text = || String::from_utf8_lossy(&written.lock().unwrap()).to_string();

        wait_for("the first drawing", || text().contains("Tests (Round 1) - 50%"));
        assert!(text().contains("  - a::one"), "{}", text());
        assert!(!text().contains("orq: verify"), "no appended log line beside the step: {}", text());

        std::fs::write(logs.join("gate.log"), "orq: tests to run: 2\nrunning 2 tests\ntest a::one ... ok\ntest a::two ... ok\n").unwrap();
        wait_for("the redraw", || text().contains("Tests (Round 1) - 100%"));
        drop(progress);
        let text = text();
        let redraw = text.split("Tests (Round 1) - 100%").next().unwrap();
        assert!(redraw.ends_with("\r\u{1b}[2K"), "the step lines are rewritten, not appended: {text:?}");
        assert!(redraw.contains("\u{1b}[2A"), "the cursor goes back up over the two lines drawn: {text:?}");
    }

    #[test]
    fn off_a_terminal_the_step_keeps_its_plain_appended_lines() {
        let (root, logs) = gate_run("plain", "orq: tests to run: 2\nrunning 2 tests\ntest a::one ... ok\n");
        let written = Arc::new(Mutex::new(Vec::new()));
        let progress = Progress::start_with(root, "run-1".into(), logs, Duration::from_secs(60), Box::new(Sink(written.clone())));
        wait_for("a line", || !written.lock().unwrap().is_empty());
        drop(progress);

        let text = String::from_utf8_lossy(&written.lock().unwrap()).to_string();
        assert!(text.contains("orq: verify") && !text.contains('\u{1b}'), "{text:?}");
    }

    struct Stalled(Arc<AtomicBool>);
    impl Write for Stalled {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            self.0.store(true, Ordering::SeqCst);
            std::thread::park();
            Ok(0)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn running_run(tag: &str) -> (PathBuf, PathBuf) {
        let root = orq_test_support::process_temp_dir().join(format!("orq-progress-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let store = RunStore::new(&root);
        let record = orq_orchestrator::RunRecord {
            run_id: "run-1".into(),
            project_id: "repo-1".into(),
            feature_id: "COM-01".into(),
            remote: "origin".into(),
            provider: "github".into(),
            target_branch: "main".into(),
            base_sha: "abc".into(),
            branch: "orq/com-01".into(),
            worktree: root.join("wt"),
            repository: String::new(),
            profiles: Default::default(),
        };
        store.start_run(&record).unwrap();
        let logs = root.join("logs");
        std::fs::create_dir_all(&logs).unwrap();
        (root, logs)
    }

    #[test]
    fn nothing_is_printed_while_a_fix_session_has_the_terminal_and_lines_resume_after_it() {
        let (root, logs) = running_run("session");
        let store = RunStore::new(&root);
        let started = |id: &str, stage: &str| {
            store
                .append_event(&orq_orchestrator::Event {
                    schema_version: 1,
                    event_id: id.into(),
                    run_id: "run-1".into(),
                    project_id: "repo-1".into(),
                    feature_id: "COM-01".into(),
                    kind: "stage.started".into(),
                    stage: Some(stage.into()),
                    attempt: 1,
                    occurred_at: "2026-11-02T10:00:00Z".into(),
                    payload: serde_json::json!({}),
                })
                .unwrap();
        };
        started("e1", "gate_fix");
        std::fs::write(logs.join("gate.log"), "failed\n").unwrap();
        let written = Arc::new(Mutex::new(Vec::new()));
        // A heartbeat of nothing would print on every look, if anything were to print.
        let progress = Progress::start_with(root.clone(), "run-1".into(), logs, Duration::ZERO, Box::new(Sink(written.clone())));

        std::thread::sleep(POLL * 3);
        assert_eq!(String::from_utf8_lossy(&written.lock().unwrap()), "");

        started("e2", "verify");
        wait_for("a line after the session", || !written.lock().unwrap().is_empty());
        drop(progress);
        assert!(String::from_utf8_lossy(&written.lock().unwrap()).contains("verify"));
    }

    #[test]
    fn a_terminal_that_stops_taking_output_never_holds_up_the_end_of_the_run() {
        let (root, logs) = running_run("stalled");
        std::fs::write(logs.join("gate.log"), "running\n").unwrap();
        let entered = Arc::new(AtomicBool::new(false));
        let progress = Progress::start_with(root, "run-1".into(), logs, Duration::from_secs(60), Box::new(Stalled(entered.clone())));
        wait_for("the write to stall", || entered.load(Ordering::SeqCst));

        let (done, finished) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            drop(progress);
            let _ = done.send(());
        });

        assert!(finished.recv_timeout(Duration::from_secs(3)).is_ok(), "stopping the printer waited for the stalled terminal");
    }

    fn activity(output: &str, quiet: u64) -> RunActivity {
        RunActivity {
            phase: orq_orchestrator::RunPhase::Running,
            stage: Some("gate".into()),
            attempt: 1,
            tests: None,
            last_output: Some(output.into()),
            may_be_stuck: false,
            quiet_for: Some(Duration::from_secs(quiet)),
        }
    }

    #[test]
    fn an_unchanged_log_reaches_the_longest_wait_however_long_it_stays_quiet() {
        let mut wait = POLL;
        let mut before = activity("running", 1);
        for quiet in 2..20 {
            let now = activity("running", quiet);
            wait = next_wait(Some(&before), &now, wait);
            before = now;
        }
        assert_eq!(wait, POLL_QUIET_MAX);
        assert_eq!(next_wait(Some(&before), &activity("42 passed", 1), wait), POLL);
    }

    struct Counting {
        writes: Arc<std::sync::atomic::AtomicUsize>,
        attempts: Arc<std::sync::atomic::AtomicUsize>,
        fail_writes: Arc<AtomicBool>,
        fail_flush: bool,
    }
    impl Write for Counting {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.fail_writes.load(Ordering::SeqCst) {
                self.attempts.fetch_add(1, Ordering::SeqCst);
                return Err(std::io::ErrorKind::BrokenPipe.into());
            }
            self.writes.fetch_add(bytes.iter().filter(|b| **b == b'\n').count(), Ordering::SeqCst);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            if self.fail_flush { Err(std::io::ErrorKind::BrokenPipe.into()) } else { Ok(()) }
        }
    }

    fn counting(writes: &Arc<std::sync::atomic::AtomicUsize>, fail_flush: bool) -> Counting {
        Counting { writes: writes.clone(), attempts: Default::default(), fail_writes: Default::default(), fail_flush }
    }

    #[test]
    fn a_write_that_errors_is_tried_again_with_the_next_line() {
        let (root, logs) = running_run("erroring");
        std::fs::write(logs.join("gate.log"), "one\n").unwrap();
        let sink = Counting { writes: Default::default(), attempts: Default::default(), fail_writes: Arc::new(AtomicBool::new(true)), fail_flush: false };
        let (attempts, writes, fail) = (sink.attempts.clone(), sink.writes.clone(), sink.fail_writes.clone());
        let progress = Progress::start_with(root, "run-1".into(), logs.clone(), Duration::from_secs(60), Box::new(sink));
        wait_for("a failed write", || attempts.load(Ordering::SeqCst) > 0);
        assert_eq!(writes.load(Ordering::SeqCst), 0);

        fail.store(false, Ordering::SeqCst);
        std::fs::write(logs.join("gate.log"), "two\n").unwrap();
        wait_for("a line after the failed write", || writes.load(Ordering::SeqCst) > 0);
        drop(progress);
    }

    fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
        let until = Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < until, "never happened: {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn nothing_is_printed_once_the_printer_has_been_dropped() {
        let (root, logs) = running_run("dropped");
        std::fs::write(logs.join("gate.log"), "first\n").unwrap();
        let writes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let progress = Progress::start_with(root, "run-1".into(), logs.clone(), Duration::from_secs(60), Box::new(counting(&writes, false)));
        wait_for("the first line", || writes.load(Ordering::SeqCst) > 0);
        drop(progress);
        let before = writes.load(Ordering::SeqCst);

        std::fs::write(logs.join("gate.log"), "second\n").unwrap();
        std::thread::sleep(Duration::from_millis(1500));

        assert_eq!(writes.load(Ordering::SeqCst), before);
    }

    #[test]
    fn a_failing_flush_and_a_run_that_appears_later_do_not_stop_the_printer() {
        let root = orq_test_support::process_temp_dir().join(format!("orq-progress-late-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let logs = root.join("logs");
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(logs.join("gate.log"), "one\n").unwrap();
        let writes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let progress = Progress::start_with(root.clone(), "run-1".into(), logs.clone(), Duration::from_secs(60), Box::new(counting(&writes, true)));
        std::thread::sleep(Duration::from_millis(700));
        assert_eq!(writes.load(Ordering::SeqCst), 0, "the run cannot be read yet");

        let (_, _) = (running_run_at(&root), ());
        wait_for("a line once the run can be read", || writes.load(Ordering::SeqCst) > 0);
        let first = writes.load(Ordering::SeqCst);
        std::fs::write(logs.join("gate.log"), "two\n").unwrap();
        wait_for("a line after a failed flush", || writes.load(Ordering::SeqCst) > first);
        drop(progress);
    }

    fn running_run_at(root: &Path) {
        let store = RunStore::new(root);
        let record = orq_orchestrator::RunRecord {
            run_id: "run-1".into(),
            project_id: "repo-1".into(),
            feature_id: "COM-01".into(),
            remote: "origin".into(),
            provider: "github".into(),
            target_branch: "main".into(),
            base_sha: "abc".into(),
            branch: "orq/com-01".into(),
            worktree: root.join("wt"),
            repository: String::new(),
            profiles: Default::default(),
        };
        store.start_run(&record).unwrap();
    }
}
