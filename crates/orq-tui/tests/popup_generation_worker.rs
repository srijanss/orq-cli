use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use orq_tui::{
    run_browser, BrowserBackend, BrowserEntry, CandidateKind, CandidateView, EventSource, GenerationJob,
    LedgerErrorView, PopupModel, PromoteOutcome,
};
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;

/// Yields `idle` empty polls before each scripted event, as a quiet terminal would.
struct Script {
    events: VecDeque<Event>,
    idle: usize,
    pending_idle: usize,
    quiet_polls: usize,
}

impl EventSource for Script {
    fn next_event(&mut self) -> io::Result<Event> {
        self.events.pop_front().ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "script exhausted"))
    }
    fn poll_event(&mut self, _timeout: Duration) -> io::Result<Option<Event>> {
        if self.pending_idle > 0 {
            self.pending_idle -= 1;
            return Ok(None);
        }
        if self.events.is_empty() {
            // A quiet terminal; bounded so a stuck generation fails the test.
            self.quiet_polls += 1;
            return if self.quiet_polls > 50 { self.next_event().map(Some) } else { Ok(None) };
        }
        self.pending_idle = self.idle;
        self.next_event().map(Some)
    }
}

struct Job {
    polls_until_done: usize,
    polled: usize,
    cancelled: Arc<AtomicBool>,
    result: Result<Vec<CandidateView>, String>,
}

impl GenerationJob for Job {
    fn poll(&mut self) -> Option<Result<Vec<CandidateView>, String>> {
        self.polled += 1;
        (self.polled > self.polls_until_done).then(|| self.result.clone())
    }
    fn cancel(&mut self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }
}

struct Backend {
    polls_until_done: usize,
    cancelled: Arc<AtomicBool>,
    started: Arc<AtomicUsize>,
    result: Result<Vec<CandidateView>, String>,
}

impl BrowserBackend for Backend {
    fn load(&mut self) -> Result<Vec<BrowserEntry>, LedgerErrorView> {
        Err(LedgerErrorView { path: "/repo/.tdd-features.json".into(), reason: "file not found".into() })
    }
    fn promote(&mut self, _feature: &str) -> PromoteOutcome {
        PromoteOutcome::Saved
    }
    fn ledger_absent(&self) -> bool {
        true
    }
    fn start_generation(&mut self) -> Result<Box<dyn GenerationJob>, String> {
        self.started.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(Job {
            polls_until_done: self.polls_until_done,
            polled: 0,
            cancelled: Arc::clone(&self.cancelled),
            result: self.result.clone(),
        }))
    }
}

fn backend(polls_until_done: usize) -> Backend {
    Backend {
        polls_until_done,
        cancelled: Arc::new(AtomicBool::new(false)),
        started: Arc::new(AtomicUsize::new(0)),
        result: Ok(vec![CandidateView {
            name: "alpha".into(),
            description: None,
            depends_on: vec![],
            kind: CandidateKind::New,
            suggests_parallel: false,
            ignored_fields: vec![],
        }]),
    }
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn screen(backend: &mut Backend, events: Vec<Event>) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let (width, height) = (100u16, 24u16);
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    let mut script = Script { events: events.into(), idle: 2, pending_idle: 0, quiet_polls: 0 };
    let _ = run_browser(&mut terminal, &model, backend, &mut script);
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| (0..width).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
        .collect()
}

fn has(lines: &[String], needle: &str) -> bool {
    lines.iter().any(|l| l.contains(needle))
}

#[test]
fn escape_cancels_a_running_generation_and_the_browser_stays_usable() {
    let mut backend = backend(usize::MAX - 1);

    // After Esc the script is exhausted, which ends the session; the browser
    // must have left the generating state by then.
    let lines = screen(&mut backend, vec![key(KeyCode::Char('a')), ctrl_g(), key(KeyCode::Esc)]);

    assert!(backend.cancelled.load(Ordering::SeqCst));
    assert!(has(&lines, "Generation cancelled"), "{lines:?}");
    assert!(has(&lines, "Add Feature"), "{lines:?}");
}

#[test]
fn a_running_generation_shows_progress_and_a_finished_one_opens_the_preview() {
    let mut backend = backend(3);

    let lines = screen(&mut backend, vec![key(KeyCode::Char('a')), ctrl_g()]);

    assert!(has(&lines, "Generated candidates"), "{lines:?}");
    assert!(has(&lines, "alpha"), "{lines:?}");
    assert!(!backend.cancelled.load(Ordering::SeqCst));
}

#[test]
fn a_failed_background_generation_is_reported() {
    let mut backend = backend(1);
    backend.result = Err("the agent exited with status 3".into());

    let lines = screen(&mut backend, vec![key(KeyCode::Char('a')), ctrl_g()]);

    assert!(has(&lines, "Generation failed: the agent exited with status 3"), "{lines:?}");
}

fn ctrl_g() -> Event {
    Event::Key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL))
}
