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

struct Fake {
    outcome: FormSaveOutcome,
    added: Vec<FeatureFields>,
    promoted: Vec<String>,
}

impl Fake {
    fn new() -> Fake {
        Fake { outcome: FormSaveOutcome::Saved, added: Vec::new(), promoted: Vec::new() }
    }
}

impl BrowserBackend for Fake {
    fn load(&mut self) -> Result<Vec<BrowserEntry>, LedgerErrorView> {
        let mut entries = vec![entry("alpha", "completed")];
        for added in &self.added {
            entries.push(entry(&added.name, "draft"));
        }
        Ok(entries)
    }
    fn promote(&mut self, feature: &str) -> PromoteOutcome {
        self.promoted.push(feature.to_string());
        PromoteOutcome::Saved
    }
    fn add_feature(&mut self, fields: &FeatureFields) -> FormSaveOutcome {
        if self.outcome == FormSaveOutcome::Saved {
            self.added.push(fields.clone());
        }
        self.outcome.clone()
    }
}

fn entry(name: &str, status: &str) -> BrowserEntry {
    BrowserEntry {
        name: name.into(),
        description: None,
        status: status.into(),
        depends_on: vec![],
        test_file: None,
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

fn run(backend: &mut Fake, events: Vec<Event>) -> (bool, Vec<String>) {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let (width, height) = (60u16, 24u16);
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    let closed = run_browser(&mut terminal, &model, backend, &mut Script(events.into())).is_ok();
    let buffer = terminal.backend().buffer();
    let lines = (0..height)
        .map(|y| (0..width).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
        .collect();
    (closed, lines)
}

/// The terminal column of `needle`, not its byte offset (borders are multi-byte).
fn column_of(line: &str, needle: &str) -> u16 {
    let byte = line.find(needle).unwrap_or_else(|| panic!("{needle} not in {line:?}"));
    line[..byte].chars().count() as u16
}

fn has(lines: &[String], needle: &str) -> bool {
    lines.iter().any(|l| l.contains(needle))
}

fn open_form() -> Vec<Event> {
    vec![key(KeyCode::Char('a'))]
}

#[test]
fn a_opens_the_form_with_every_field_and_a_fixed_draft_status() {
    let (_, lines) = run(&mut Fake::new(), open_form());

    for needle in ["Add Feature", "Name", "Description", "draft", "Parallelizable", "Default", "Test file", "Target files", "Depends on", "alpha"] {
        assert!(has(&lines, needle), "missing {needle}: {lines:?}");
    }
}

#[test]
fn the_add_feature_button_opens_the_form() {
    let (_, lines) = run(&mut Fake::new(), vec![]);
    let column = column_of(&lines[2], "Add Feature");

    let (_, lines) = run(&mut Fake::new(), vec![click(column, 2)]);

    assert!(has(&lines, "Depends on"), "{lines:?}");
}

#[test]
fn a_name_alone_saves_a_minimal_draft_and_returns_to_the_list() {
    let mut backend = Fake::new();
    let mut events = open_form();
    events.extend(typed("beta"));
    events.extend([key(KeyCode::Down), key(KeyCode::Down), key(KeyCode::Down), key(KeyCode::Down)]);
    events.extend([key(KeyCode::Down), key(KeyCode::Down), key(KeyCode::Enter)]);

    let (_, lines) = run(&mut backend, events);

    assert_eq!(backend.added.len(), 1, "{lines:?}");
    let saved = &backend.added[0];
    assert_eq!(saved.name, "beta");
    assert_eq!(saved.description, None);
    assert_eq!(saved.parallelizable, None);
    assert_eq!(saved.test_file, None);
    assert!(saved.depends_on.is_empty() && saved.target_files.is_empty());
    assert!(!has(&lines, "Depends on"), "form should be closed: {lines:?}");
    assert!(has(&lines, "beta"), "{lines:?}");
}

#[test]
fn every_field_is_collected() {
    let mut backend = Fake::new();
    let mut events = open_form();
    events.extend(typed("beta"));
    events.push(key(KeyCode::Down));
    events.extend(typed("does a thing"));
    events.push(key(KeyCode::Down));
    events.push(key(KeyCode::Char(' ')));
    events.push(key(KeyCode::Down));
    events.extend(typed("tests/beta.rs"));
    events.push(key(KeyCode::Down));
    events.extend(typed("src/a.rs, src/b.rs"));
    events.push(key(KeyCode::Down));
    events.push(key(KeyCode::Char(' ')));
    events.push(key(KeyCode::Char('s')));
    // Ctrl-S from a dependency row saves.
    let last = events.len() - 1;
    events[last] = Event::Key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));

    run(&mut backend, events);

    assert_eq!(
        backend.added,
        vec![FeatureFields {
            name: "beta".into(),
            description: Some("does a thing".into()),
            depends_on: vec!["alpha".into()],
            parallelizable: Some(true),
            test_file: Some("tests/beta.rs".into()),
            target_files: vec!["src/a.rs".into(), "src/b.rs".into()],
        }]
    );
}

#[test]
fn a_rejected_save_shows_the_reason_inline_and_keeps_the_form_open() {
    let mut backend = Fake::new();
    backend.outcome = FormSaveOutcome::Invalid { reason: "featureName \"beta\" is repeated".into() };
    let mut events = open_form();
    events.extend(typed("beta"));
    events.push(Event::Key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)));

    let (_, lines) = run(&mut backend, events);

    assert!(has(&lines, "is repeated"), "{lines:?}");
    assert!(has(&lines, "Depends on"), "form should stay open: {lines:?}");
    assert!(backend.added.is_empty());
}

#[test]
fn esc_cancels_without_saving_or_closing_the_popup() {
    let mut backend = Fake::new();
    let mut events = open_form();
    events.extend(typed("beta"));
    events.push(key(KeyCode::Esc));

    let (closed, lines) = run(&mut backend, events);

    assert!(!closed, "the popup must stay open");
    assert!(backend.added.is_empty());
    assert!(!has(&lines, "Depends on"), "{lines:?}");
    assert!(has(&lines, "alpha"), "{lines:?}");
}

#[test]
fn clicking_save_saves() {
    let (_, lines) = run(&mut Fake::new(), open_form());
    let row = lines.iter().position(|l| l.contains("Save")).expect("save button") as u16;
    let column = column_of(&lines[row as usize], "Save");
    let mut backend = Fake::new();
    let mut events = open_form();
    events.extend(typed("beta"));
    events.push(click(column, row));

    run(&mut backend, events);

    assert_eq!(backend.added.len(), 1);
}

#[test]
fn saving_never_promotes_or_starts_anything() {
    let mut backend = Fake::new();
    let mut events = open_form();
    events.extend(typed("beta"));
    events.push(Event::Key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)));

    run(&mut backend, events);

    assert!(backend.promoted.is_empty());
}
