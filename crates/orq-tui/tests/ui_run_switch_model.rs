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
    id: &'static str,
    reply: Option<Result<String, String>>,
    calls: Vec<String>,
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
            run_id: if self.id.is_empty() { "run-1" } else { self.id }.into(),
            feature: "COM-01".into(),
            status: self.status.into(),
            stage: Some("gate".into()),
            implementer_model: None,
            reviewer_model: None,
            has_review_link: true,
            reason: None,
        }])
    }
    fn switch_model(&mut self, run_id: &str, role: &str, profile: &str) -> Result<String, String> {
        self.calls.push(format!("switch {run_id} {role} {profile}"));
        self.reply.clone().unwrap()
    }
}

fn keys(chars: &str) -> Vec<Event> {
    let mut events = vec![Event::Key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::NONE))];
    events.extend(chars.chars().map(|c| Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))));
    events
}

fn code(c: KeyCode) -> Event {
    Event::Key(KeyEvent::new(c, KeyModifiers::NONE))
}

fn show(backend: &mut Fake, events: Vec<Event>) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let (width, height) = (100u16, 24u16);
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    let _ = run_browser(&mut terminal, &model, backend, &mut Script(events.into()));
    let buffer = terminal.backend().buffer();
    (0..height).map(|y| (0..width).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string()).collect()
}

fn has(lines: &[String], needle: &str) -> bool {
    lines.iter().any(|l| l.contains(needle))
}

#[test]
fn m_on_a_paused_run_records_the_typed_role_and_profile_through_the_backend() {
    let mut backend = Fake { status: "paused", reply: Some(Ok("Switch recorded for run-1".into())), ..Fake::default() };
    let mut events = keys("mimplementer fast-claude");
    events.push(code(KeyCode::Enter));
    let lines = show(&mut backend, events);
    assert_eq!(backend.calls, ["switch run-1 implementer fast-claude"]);
    assert!(has(&lines, "Switch recorded for run-1"), "{lines:?}");
}

#[test]
fn the_backend_reason_for_an_unknown_profile_is_shown() {
    let mut backend = Fake { status: "paused", reply: Some(Err("unknown profile nope".into())), ..Fake::default() };
    let mut events = keys("mimplementer nope");
    events.push(code(KeyCode::Enter));
    let lines = show(&mut backend, events);
    assert!(has(&lines, "unknown profile nope"), "{lines:?}");
}

#[test]
fn m_on_a_run_that_is_not_paused_does_not_reach_the_backend() {
    let mut backend = Fake { status: "running", reply: Some(Ok(String::new())), ..Fake::default() };
    let lines = show(&mut backend, keys("m"));
    assert!(backend.calls.is_empty());
    assert!(has(&lines, "only a paused run can switch models"), "{lines:?}");
}

#[test]
fn esc_cancels_and_an_incomplete_entry_is_not_sent() {
    let mut backend = Fake { status: "paused", reply: Some(Ok(String::new())), ..Fake::default() };
    let mut events = keys("mimplementer");
    events.push(code(KeyCode::Enter));
    let lines = show(&mut backend, events);
    assert!(backend.calls.is_empty());
    assert!(has(&lines, "type a role and a profile"), "{lines:?}");

    let mut backend = Fake { status: "paused", reply: Some(Ok(String::new())), ..Fake::default() };
    let mut events = keys("mx");
    events.push(code(KeyCode::Esc));
    let lines = show(&mut backend, events);
    assert!(backend.calls.is_empty());
    assert!(has(&lines, "Did not switch run-1"), "{lines:?}");
}

#[test]
fn the_hint_offers_the_switch_on_a_paused_run() {
    let mut backend = Fake { status: "paused", ..Fake::default() };
    assert!(has(&show(&mut backend, keys("")), "m switch model"));
}

#[test]
fn the_typed_text_is_visible_at_80_columns_with_a_long_run_id() {
    let mut backend = Fake { status: "paused", reply: Some(Ok(String::new())), ..Fake::default() };
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let _ = run_browser(&mut terminal, &model, &mut backend, &mut Script(keys("mimplementer fast").into()));
    let buffer = terminal.backend().buffer();
    let lines: Vec<String> = (0..24u16).map(|y| (0..80u16).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>()).collect();
    assert!(has(&lines, "implementer fast"), "{lines:?}");
}

#[test]
fn the_typed_text_stays_visible_when_the_run_id_alone_fills_the_row() {
    let long_id: &'static str = "run-20261107T101500Z-0123456789abcdef0123456789abcdef-feature";
    let mut backend = Fake { status: "paused", id: long_id, reply: Some(Ok(String::new())), ..Fake::default() };
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(TestBackend::new(60, 24)).unwrap();
    let _ = run_browser(&mut terminal, &model, &mut backend, &mut Script(keys("mimplementer fast").into()));
    let buffer = terminal.backend().buffer();
    let lines: Vec<String> = (0..24u16).map(|y| (0..60u16).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>()).collect();
    assert!(has(&lines, "implementer fast_"), "{lines:#?}");
}
