use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use orq_tui::{
    run_browser, BrowserBackend, BrowserEntry, CandidateView, EventSource, GenerationJob, LedgerErrorView,
    PopupModel, PromoteOutcome,
};
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;

struct Script(VecDeque<Event>);

impl EventSource for Script {
    fn next_event(&mut self) -> io::Result<Event> {
        self.0.pop_front().ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "script exhausted"))
    }
    fn poll_event(&mut self, _timeout: Duration) -> io::Result<Option<Event>> {
        self.next_event().map(Some)
    }
}

struct Idle;

impl GenerationJob for Idle {
    fn poll(&mut self) -> Option<Result<Vec<CandidateView>, String>> {
        None
    }
    fn cancel(&mut self) {}
}

struct Backend {
    absent: bool,
    started: Arc<Mutex<Vec<(String, String)>>>,
}

impl BrowserBackend for Backend {
    fn feature_fields(&mut self, name: &str) -> Option<orq_tui::FeatureFields> {
        Some(orq_tui::FeatureFields { name: name.to_string(), ..Default::default() })
    }

    fn load(&mut self) -> Result<Vec<BrowserEntry>, LedgerErrorView> {
        if self.absent {
            return Err(LedgerErrorView { path: "/repo/.tdd-features.json".into(), reason: "file not found".into() });
        }
        Ok(vec![BrowserEntry {
            name: "alpha".into(),
            description: None,
            status: "draft".into(),
            depends_on: vec![],
            test_file: None,
        }])
    }
    fn promote(&mut self, _feature: &str) -> PromoteOutcome {
        PromoteOutcome::Saved
    }
    fn ledger_absent(&self) -> bool {
        self.absent
    }
    fn spec_files(&mut self) -> Vec<String> {
        vec!["SPEC.md".into()]
    }
    fn start_generation_for(&mut self, spec: &str, focus: &str) -> Result<Box<dyn GenerationJob>, String> {
        self.started.lock().unwrap().push((spec.to_string(), focus.to_string()));
        Ok(Box::new(Idle))
    }
}

fn backend(absent: bool) -> Backend {
    Backend { absent, started: Arc::default() }
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn ctrl(c: char) -> Event {
    Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
}

fn screen(backend: &mut Backend, events: Vec<Event>) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let (width, height) = (100u16, 24u16);
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    let mut script = Script(events.into());
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
fn the_add_form_hint_advertises_generate_from_spec() {
    let lines = screen(&mut backend(false), vec![key(KeyCode::Char('a'))]);

    assert!(has(&lines, "Ctrl+G generate"), "{lines:?}");
}

#[test]
fn ctrl_g_in_the_add_form_opens_the_spec_picker() {
    let mut backend = backend(false);

    let lines = screen(&mut backend, vec![key(KeyCode::Char('a')), ctrl('g')]);

    assert!(has(&lines, "Section"), "{lines:?}");
    assert!(!has(&lines, "Depends on"), "{lines:?}");
}

#[test]
fn typing_g_in_the_form_is_just_a_letter() {
    let mut backend = backend(false);

    let lines = screen(&mut backend, vec![key(KeyCode::Char('a')), key(KeyCode::Char('g'))]);

    assert!(has(&lines, "Depends on"), "{lines:?}");
    assert!(!has(&lines, "Section"), "{lines:?}");
}

#[test]
fn g_in_the_list_no_longer_starts_generation() {
    let mut backend = backend(false);

    let lines = screen(&mut backend, vec![key(KeyCode::Char('g'))]);

    assert!(!has(&lines, "Section"), "{lines:?}");
    assert!(backend.started.lock().unwrap().is_empty());
}

#[test]
fn g_in_the_empty_state_no_longer_starts_generation_and_no_button_is_shown() {
    let mut backend = backend(true);

    let lines = screen(&mut backend, vec![key(KeyCode::Char('g'))]);

    assert!(!has(&lines, "Section"), "{lines:?}");
    assert!(!has(&lines, "Generate from Spec"), "{lines:?}");
    assert!(has(&lines, "Add Feature"), "{lines:?}");
}

#[test]
fn ctrl_g_in_the_edit_form_does_nothing() {
    let mut backend = backend(false);

    let lines = screen(&mut backend, vec![key(KeyCode::Down), key(KeyCode::Char('e')), ctrl('g')]);

    assert!(has(&lines, "Edit Feature"), "{lines:?}");
    assert!(!has(&lines, "Section"), "{lines:?}");
    assert!(!has(&lines, "Ctrl+G"), "{lines:?}");
}
