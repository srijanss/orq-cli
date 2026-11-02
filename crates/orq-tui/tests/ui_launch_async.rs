use std::collections::VecDeque;
use std::io;

use orq_tui::{
    run_browser, BrowserBackend, BrowserEntry, EventSource, LaunchJob, LedgerErrorView, PopupModel, PromoteOutcome,
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

/// A launch that stays running for `polls` polls and then reports `result`.
struct SlowLaunch {
    polls: usize,
    result: Result<String, String>,
}

impl LaunchJob for SlowLaunch {
    fn poll(&mut self) -> Option<Result<String, String>> {
        if self.polls == 0 {
            return Some(self.result.clone());
        }
        self.polls -= 1;
        None
    }
}

struct Fake {
    started: Vec<String>,
    polls: usize,
    result: Result<String, String>,
}

impl BrowserBackend for Fake {
    fn load(&mut self) -> Result<Vec<BrowserEntry>, LedgerErrorView> {
        Ok(vec![BrowserEntry {
            name: "alpha".into(),
            description: None,
            status: "pending".into(),
            depends_on: vec![],
            test_file: None,
        }])
    }
    fn promote(&mut self, _feature: &str) -> PromoteOutcome {
        PromoteOutcome::Saved
    }
    fn launch_profiles(&mut self) -> Result<Vec<String>, String> {
        Ok(vec!["opus".into(), "sonnet".into()])
    }
    fn start_launch(&mut self, feature: &str, _implementer: &str, _reviewer: &str) -> Box<dyn LaunchJob> {
        self.started.push(feature.into());
        Box::new(SlowLaunch { polls: self.polls, result: self.result.clone() })
    }
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn launch_keys() -> Vec<Event> {
    vec![key(KeyCode::Down), key(KeyCode::Char('L')), key(KeyCode::Enter), key(KeyCode::Enter)]
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
fn a_launch_in_progress_is_announced() {
    let mut backend = Fake { started: vec![], polls: 100, result: Ok("run-7".into()) };

    let lines = show(&mut backend, launch_keys());

    assert_eq!(backend.started, vec!["alpha".to_string()]);
    assert!(has(&lines, "Launching alpha"), "{lines:?}");
}

#[test]
fn the_popup_takes_keys_while_a_launch_is_still_running() {
    let mut backend = Fake { started: vec![], polls: 100, result: Ok("run-7".into()) };
    let mut events = launch_keys();
    events.push(key(KeyCode::Char('R')));

    let lines = show(&mut backend, events);

    assert!(has(&lines, "runs are not available"), "the Runs view opened: {lines:?}");
}

#[test]
fn the_notice_names_the_run_once_the_launch_finishes_and_the_reason_when_it_fails() {
    let mut ok = Fake { started: vec![], polls: 1, result: Ok("run-7 (no Herdr pane: x)".into()) };
    let mut events = launch_keys();
    events.push(key(KeyCode::Down));
    let lines = show(&mut ok, events);
    assert!(has(&lines, "Started run run-7"), "{lines:?}");

    let mut bad = Fake { started: vec![], polls: 0, result: Err("feature is already running".into()) };
    let lines = show(&mut bad, launch_keys());
    assert!(has(&lines, "feature is already running"), "{lines:?}");
}

#[test]
fn a_second_launch_is_refused_while_the_first_is_still_running() {
    let mut backend = Fake { started: vec![], polls: 100, result: Ok("run-7".into()) };
    let mut events = launch_keys();
    events.extend([key(KeyCode::Char('L'))]);

    let lines = show(&mut backend, events);

    assert!(has(&lines, "A launch is still running"), "{lines:?}");
    assert!(!has(&lines, "choose the implementer"), "no second picker: {lines:?}");
    assert_eq!(backend.started.len(), 1);
}
