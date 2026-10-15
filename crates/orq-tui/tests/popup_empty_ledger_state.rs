use std::collections::VecDeque;
use std::io;

use orq_tui::{
    run_browser, BrowserBackend, BrowserEntry, EventSource, FeatureFields, FormSaveOutcome, LedgerErrorView,
    PopupModel, PromoteOutcome,
};
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Terminal;

struct Script(VecDeque<Event>);

impl EventSource for Script {
    fn next_event(&mut self) -> io::Result<Event> {
        self.0.pop_front().ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "script exhausted"))
    }
}

const PATH: &str = "/repo/.tdd-features.json";

struct Fake {
    absent: bool,
    reason: String,
    added: Vec<FeatureFields>,
}

impl Fake {
    fn absent() -> Fake {
        Fake { absent: true, reason: "file not found".into(), added: Vec::new() }
    }

    fn malformed() -> Fake {
        Fake { absent: false, reason: "expected a JSON array of features".into(), added: Vec::new() }
    }
}

impl BrowserBackend for Fake {
    fn load(&mut self) -> Result<Vec<BrowserEntry>, LedgerErrorView> {
        if self.added.is_empty() {
            return Err(LedgerErrorView { path: PATH.into(), reason: self.reason.clone() });
        }
        Ok(self
            .added
            .iter()
            .map(|f| BrowserEntry {
                name: f.name.clone(),
                description: None,
                status: "draft".into(),
                depends_on: vec![],
                test_file: None,
            })
            .collect())
    }
    fn promote(&mut self, _feature: &str) -> PromoteOutcome {
        PromoteOutcome::Saved
    }
    fn ledger_absent(&self) -> bool {
        self.absent && self.added.is_empty()
    }
    fn add_feature(&mut self, fields: &FeatureFields) -> FormSaveOutcome {
        self.added.push(fields.clone());
        FormSaveOutcome::Saved
    }
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn typed(text: &str) -> Vec<Event> {
    text.chars().map(|c| key(KeyCode::Char(c))).collect()
}

fn click(column: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

fn run(backend: &mut Fake, events: Vec<Event>) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let (width, height) = (60u16, 24u16);
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    let _ = run_browser(&mut terminal, &model, backend, &mut Script(events.into()));
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| (0..width).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
        .collect()
}

fn has(lines: &[String], needle: &str) -> bool {
    lines.iter().any(|l| l.contains(needle))
}

fn position_of(lines: &[String], needle: &str, from_row: usize) -> (u16, u16) {
    let row = (from_row..lines.len()).find(|&r| lines[r].contains(needle)).expect(needle);
    let byte = lines[row].find(needle).unwrap();
    (lines[row][..byte].chars().count() as u16, row as u16)
}

#[test]
fn an_absent_file_shows_an_empty_state_with_both_actions() {
    let lines = run(&mut Fake::absent(), vec![]);

    assert!(has(&lines, "No feature file yet"), "{lines:?}");
    assert!(has(&lines, PATH), "{lines:?}");
    assert!(has(&lines, "Add Feature"), "{lines:?}");
    assert!(has(&lines, "Generate from Spec"), "{lines:?}");
    assert!(!has(&lines, "ledger error"), "{lines:?}");
}

#[test]
fn a_opens_the_form_and_saving_creates_the_first_feature() {
    let mut backend = Fake::absent();
    let mut events = vec![key(KeyCode::Char('a'))];
    events.extend(typed("first"));
    events.push(Event::Key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)));

    let lines = run(&mut backend, events);

    assert_eq!(backend.added.len(), 1);
    assert_eq!(backend.added[0].name, "first");
    assert!(has(&lines, "first"), "{lines:?}");
    assert!(!has(&lines, "No feature file yet"), "{lines:?}");
}

#[test]
fn the_add_button_in_the_empty_state_opens_the_form() {
    let lines = run(&mut Fake::absent(), vec![]);
    let (column, row) = position_of(&lines, "Add Feature", 5);

    let lines = run(&mut Fake::absent(), vec![click(column, row)]);

    assert!(has(&lines, "Depends on"), "{lines:?}");
}

#[test]
fn generate_from_spec_without_a_generating_backend_reports_it_and_keeps_manual_creation() {
    let lines = run(&mut Fake::absent(), vec![]);
    let (column, row) = position_of(&lines, "Generate from Spec", 5);

    let after = run(&mut Fake::absent(), vec![click(column, row)]);

    assert!(!has(&lines, "not available yet"), "{lines:?}");
    assert!(has(&after, "Generation failed"), "{after:?}");
    assert!(has(&after, "No feature file yet"), "{after:?}");
    assert!(has(&after, "Add Feature"), "{after:?}");
    assert!(!has(&after, "Depends on"), "{after:?}");
}

#[test]
fn a_malformed_file_shows_its_error_and_offers_no_write_action() {
    let mut backend = Fake::malformed();

    let lines = run(&mut backend, vec![key(KeyCode::Char('a'))]);

    assert!(has(&lines, "expected a JSON array"), "{lines:?}");
    assert!(has(&lines, PATH), "{lines:?}");
    assert!(!has(&lines, "Add Feature"), "{lines:?}");
    assert!(!has(&lines, "Generate from Spec"), "{lines:?}");
    assert!(!has(&lines, "Depends on"), "{lines:?}");
    assert!(backend.added.is_empty());
}
