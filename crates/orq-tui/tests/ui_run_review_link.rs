use std::collections::VecDeque;
use std::io;

use orq_tui::{
    run_browser, BrowserBackend, BrowserEntry, EventSource, LedgerErrorView, PopupModel, PromoteOutcome,
    RunRow,
};
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;

struct Script(VecDeque<Event>);

impl EventSource for Script {
    fn next_event(&mut self) -> io::Result<Event> {
        self.0.pop_front().ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "script exhausted"))
    }
}

struct Fake {
    calls: Vec<String>,
    answer: Result<String, String>,
}

impl BrowserBackend for Fake {
    fn load(&mut self) -> Result<Vec<BrowserEntry>, LedgerErrorView> {
        Ok(vec![])
    }
    fn promote(&mut self, _feature: &str) -> PromoteOutcome {
        PromoteOutcome::Saved
    }
    fn load_runs(&mut self) -> Result<Vec<RunRow>, String> {
        Ok(vec![RunRow {
            run_id: "run-1".into(),
            feature: "COM-01".into(),
            status: "running".into(),
            stage: None,
            implementer_model: None,
            reviewer_model: None,
        }])
    }
    fn open_review_link(&mut self, run_id: &str) -> Result<String, String> {
        self.calls.push(format!("open {run_id}"));
        self.answer.clone()
    }
    fn copy_review_link(&mut self, run_id: &str) -> Result<String, String> {
        self.calls.push(format!("copy {run_id}"));
        self.answer.clone()
    }
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn show(backend: &mut Fake, events: Vec<Event>) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let _ = run_browser(&mut terminal, &model, backend, &mut Script(events.into()));
    let buffer = terminal.backend().buffer();
    (0..24u16)
        .map(|y| (0..80u16).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
        .collect()
}

fn has(lines: &[String], needle: &str) -> bool {
    lines.iter().any(|l| l.contains(needle))
}

#[test]
fn u_opens_and_y_copies_the_selected_runs_review_link_and_says_what_happened() {
    let mut backend = Fake { calls: vec![], answer: Ok("Opened https://example.test/pr/1".into()) };

    let lines = show(&mut backend, vec![key(KeyCode::Char('R')), key(KeyCode::Char('u'))]);
    assert!(has(&lines, "Opened https://example.test/pr/1"), "{lines:?}");

    let lines = show(&mut backend, vec![key(KeyCode::Char('R')), key(KeyCode::Char('y'))]);
    assert!(has(&lines, "Opened https://example.test/pr/1"), "{lines:?}");
    assert_eq!(backend.calls, vec!["open run-1".to_string(), "copy run-1".to_string()]);
}

#[test]
fn a_run_without_a_review_link_says_why_instead() {
    let mut backend = Fake { calls: vec![], answer: Err("run-1 has no review request yet".into()) };

    let lines = show(&mut backend, vec![key(KeyCode::Char('R')), key(KeyCode::Char('y'))]);

    assert!(has(&lines, "has no review request yet"), "{lines:?}");
}
