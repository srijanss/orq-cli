//! Short activity lines on the terminal of a run orq started interactively, for the stages that
//! follow the agent session: without them the pane looks frozen while the gate, push and CI run.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use orq_orchestrator::{activity_line, newest_log_tail, run_activity, RunActivity, RunStore};

const POLL: Duration = Duration::from_millis(500);
const HEARTBEAT: Duration = Duration::from_secs(60);
const STUCK_AFTER: Duration = Duration::from_secs(300);

/// Prints activity lines until dropped. Printing never fails the run.
pub struct Progress {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Progress {
    pub fn start(state_root: PathBuf, run_id: String, logs: PathBuf) -> Self {
        Self::start_with(state_root, run_id, logs, HEARTBEAT, Box::new(std::io::stderr()))
    }

    pub fn start_with(state_root: PathBuf, run_id: String, logs: PathBuf, heartbeat: Duration, mut out: Box<dyn Write + Send>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::spawn(move || {
            let store = RunStore::new(&state_root);
            let mut previous: Option<RunActivity> = None;
            let mut last_line = Instant::now();
            loop {
                if let Some(current) = look(&store, &run_id, &logs) {
                    if let Some(line) = activity_line(previous.as_ref(), &current, last_line.elapsed(), heartbeat) {
                        let stuck = if current.may_be_stuck { " (it may be stuck)" } else { "" };
                        let _ = writeln!(out, "{line}{stuck}");
                        let _ = out.flush();
                        last_line = Instant::now();
                    }
                    previous = Some(current);
                }
                if flag.load(Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(POLL);
            }
        });
        Self { stop, thread: Some(thread) }
    }
}

fn look(store: &RunStore, run_id: &str, logs: &Path) -> Option<RunActivity> {
    let events = store.events(run_id).ok()?;
    let tail = newest_log_tail(logs);
    Some(run_activity(&events, tail.as_ref(), STUCK_AFTER))
}

impl Drop for Progress {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
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
}
