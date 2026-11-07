use std::collections::VecDeque;
use std::io;

use orq_tui::{run_browser, BrowserBackend, BrowserEntry, EventSource, LedgerErrorView, PopupModel, PromoteOutcome, RunRow};
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;

struct Script(VecDeque<Event>);

impl EventSource for Script {
    fn next_event(&mut self) -> io::Result<Event> {
        self.0.pop_front().ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "script exhausted"))
    }
}

#[derive(Default)]
struct Fake {
    status: &'static str,
    check: Option<Result<String, String>>,
    calls: Vec<String>,
    no_link: bool,
}

impl BrowserBackend for Fake {
    fn load(&mut self) -> Result<Vec<BrowserEntry>, LedgerErrorView> {
        Ok(vec![BrowserEntry { name: "alpha".into(), description: None, status: "pending".into(), depends_on: vec![], test_file: None }])
    }
    fn promote(&mut self, _feature: &str) -> PromoteOutcome {
        PromoteOutcome::Saved
    }
    fn load_runs(&mut self) -> Result<Vec<RunRow>, String> {
        Ok(vec![RunRow {
            run_id: "run-1".into(),
            feature: "COM-01".into(),
            status: self.status.into(),
            stage: Some("gate".into()),
            implementer_model: None,
            reviewer_model: None,
            has_review_link: !self.no_link,
            reason: None,
        }])
    }
    fn pause_run(&mut self, run_id: &str) -> Result<String, String> {
        self.calls.push(format!("pause {run_id}"));
        Ok(format!("Pause requested for {run_id}"))
    }
    fn resume_check(&mut self, run_id: &str) -> Result<String, String> {
        self.calls.push(format!("check {run_id}"));
        self.check.clone().unwrap()
    }
    fn resume_run(&mut self, run_id: &str) -> Result<String, String> {
        self.calls.push(format!("resume {run_id}"));
        Ok(format!("Resuming {run_id}"))
    }
}

fn open_runs() -> Vec<Event> {
    vec![Event::Key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::NONE))]
}

fn keys_only(chars: &str) -> Vec<Event> {
    chars.chars().map(|c| Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))).collect()
}

fn show(backend: &mut Fake, events: Vec<Event>) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let (width, height) = (80u16, 24u16);
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    let _ = run_browser(&mut terminal, &model, backend, &mut Script(events.into()));
    let buffer = terminal.backend().buffer();
    (0..height).map(|y| (0..width).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string()).collect()
}

fn has(lines: &[String], needle: &str) -> bool {
    lines.iter().any(|l| l.contains(needle))
}

#[test]
fn p_asks_a_running_run_to_pause_and_says_so() {
    let mut backend = Fake { status: "running", ..Fake::default() };
    let lines = show(&mut backend, [open_runs(), keys_only("p")].concat());
    assert_eq!(backend.calls, ["pause run-1"]);
    assert!(has(&lines, "Pause requested for run-1"), "{lines:?}");
}

#[test]
fn p_on_a_run_that_is_not_running_does_not_reach_the_backend() {
    let mut backend = Fake { status: "failed", ..Fake::default() };
    let lines = show(&mut backend, [open_runs(), keys_only("p")].concat());
    assert!(backend.calls.is_empty());
    assert!(has(&lines, "only a running run can be paused"), "{lines:?}");
}

#[test]
fn c_checks_the_checkpoint_and_resumes_only_after_y() {
    let mut backend = Fake { status: "paused", check: Some(Ok("rebase".into())), ..Fake::default() };
    let lines = show(&mut backend, [open_runs(), keys_only("c")].concat());
    assert!(has(&lines, "Resume run-1 from rebase? y to confirm"), "{lines:?}");
    assert_eq!(backend.calls, ["check run-1"]);

    let mut backend = Fake { status: "paused", check: Some(Ok("rebase".into())), ..Fake::default() };
    let lines = show(&mut backend, [open_runs(), keys_only("cy")].concat());
    assert_eq!(backend.calls, ["check run-1", "resume run-1"]);
    assert!(has(&lines, "Resuming run-1"), "{lines:?}");
}

#[test]
fn any_other_key_cancels_the_resume() {
    let mut backend = Fake { status: "paused", check: Some(Ok("rebase".into())), ..Fake::default() };
    let lines = show(&mut backend, [open_runs(), keys_only("cn")].concat());
    assert_eq!(backend.calls, ["check run-1"]);
    assert!(has(&lines, "Did not resume run-1"), "{lines:?}");
}

#[test]
fn an_invalid_checkpoint_shows_the_reason_and_cannot_be_confirmed() {
    let mut backend = Fake { status: "paused", check: Some(Err("run-1: HEAD moved since the pause".into())), ..Fake::default() };
    let lines = show(&mut backend, [open_runs(), keys_only("cy")].concat());
    assert_eq!(backend.calls, ["check run-1"]);
    assert!(!has(&lines, "Resuming"), "{lines:?}");
}

#[test]
fn the_hint_offers_the_control_that_fits_the_run() {
    let mut backend = Fake { status: "running", ..Fake::default() };
    assert!(has(&show(&mut backend, open_runs()), "p pause"));
    let mut backend = Fake { status: "paused", ..Fake::default() };
    assert!(has(&show(&mut backend, open_runs()), "c resume"));
}

#[test]
fn the_pause_and_resume_hints_show_even_when_the_run_has_no_review_link() {
    let mut backend = Fake { status: "running", no_link: true, ..Fake::default() };
    assert!(has(&show(&mut backend, open_runs()), "p pause"));
    let mut backend = Fake { status: "paused", no_link: true, ..Fake::default() };
    assert!(has(&show(&mut backend, open_runs()), "c resume"));
}

#[test]
fn c_on_a_failed_run_asks_to_resume_it_and_resumes_after_y() {
    for status in ["failed", "blocked"] {
        let mut backend = Fake { status, check: Some(Ok("where it stopped".into())), ..Fake::default() };
        let lines = show(&mut backend, [open_runs(), keys_only("cy")].concat());
        assert_eq!(backend.calls, ["check run-1", "resume run-1"], "{status}");
        assert!(has(&lines, "Resuming run-1"), "{status}: {lines:?}");
    }
}

#[test]
fn the_runs_hint_offers_resume_for_failed_and_blocked_runs() {
    for status in ["failed", "blocked", "paused"] {
        let mut backend = Fake { status, ..Fake::default() };
        let lines = show(&mut backend, open_runs());
        assert!(has(&lines, "c resume"), "{status}: {lines:?}");
    }
}
