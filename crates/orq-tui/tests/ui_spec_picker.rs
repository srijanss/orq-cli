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
    files: Vec<String>,
    started: Arc<Mutex<Vec<(String, String)>>>,
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
    fn spec_files(&mut self) -> Vec<String> {
        self.files.clone()
    }
    fn start_generation_for(&mut self, spec: &str, focus: &str) -> Result<Box<dyn GenerationJob>, String> {
        self.started.lock().unwrap().push((spec.to_string(), focus.to_string()));
        Ok(Box::new(Idle))
    }
}

fn backend(files: &[&str]) -> Backend {
    Backend { files: files.iter().map(|f| f.to_string()).collect(), started: Arc::default() }
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn typed(text: &str) -> Vec<Event> {
    text.chars().map(|c| key(KeyCode::Char(c))).collect()
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
fn generate_lists_the_markdown_files_and_filters_them_as_you_type() {
    let mut backend = backend(&["SPEC.md", "docs/mvp2.md", "notes.md"]);

    let mut events = vec![key(KeyCode::Char('g'))];
    events.extend(typed("mvp"));
    let lines = screen(&mut backend, events);

    assert!(has(&lines, "docs/mvp2.md"), "{lines:?}");
    assert!(!has(&lines, "notes.md"), "{lines:?}");
    assert!(!has(&lines, "SPEC.md"), "{lines:?}");
    assert!(has(&lines, "Section"), "{lines:?}");
    assert!(backend.started.lock().unwrap().is_empty());
}

#[test]
fn enter_starts_generation_with_the_chosen_file_and_the_section_text() {
    let mut backend = backend(&["SPEC.md", "docs/mvp2.md", "notes.md"]);

    let mut events = vec![key(KeyCode::Char('g'))];
    events.extend(typed("notes"));
    events.push(key(KeyCode::Tab));
    events.extend(typed("MVP2"));
    events.push(key(KeyCode::Enter));
    let lines = screen(&mut backend, events);

    assert_eq!(*backend.started.lock().unwrap(), [("notes.md".to_string(), "MVP2".to_string())]);
    assert!(has(&lines, "Generating"), "{lines:?}");
}

#[test]
fn arrow_keys_choose_a_file_and_a_blank_section_means_the_whole_spec() {
    let mut backend = backend(&["SPEC.md", "notes.md"]);

    let events = vec![key(KeyCode::Char('g')), key(KeyCode::Down), key(KeyCode::Enter)];
    let _ = screen(&mut backend, events);

    assert_eq!(*backend.started.lock().unwrap(), [("notes.md".to_string(), String::new())]);
}

#[test]
fn escape_closes_the_picker_without_starting_anything() {
    let mut backend = backend(&["SPEC.md"]);

    let lines = screen(&mut backend, vec![key(KeyCode::Char('g')), key(KeyCode::Esc)]);

    assert!(backend.started.lock().unwrap().is_empty());
    assert!(!has(&lines, "Section"), "{lines:?}");
}

#[test]
fn no_matching_file_starts_nothing() {
    let mut backend = backend(&["SPEC.md"]);

    let mut events = vec![key(KeyCode::Char('g'))];
    events.extend(typed("zzz"));
    events.push(key(KeyCode::Enter));
    let lines = screen(&mut backend, events);

    assert!(backend.started.lock().unwrap().is_empty());
    assert!(has(&lines, "No matching"), "{lines:?}");
}

struct Failing(Option<String>);

impl GenerationJob for Failing {
    fn poll(&mut self) -> Option<Result<Vec<CandidateView>, String>> {
        self.0.take().map(Err)
    }
    fn cancel(&mut self) {}
}

struct Flaky {
    files: Vec<String>,
    attempts: Arc<Mutex<u32>>,
}

impl BrowserBackend for Flaky {
    fn load(&mut self) -> Result<Vec<BrowserEntry>, LedgerErrorView> {
        Err(LedgerErrorView { path: "/repo/.tdd-features.json".into(), reason: "file not found".into() })
    }
    fn promote(&mut self, _feature: &str) -> PromoteOutcome {
        PromoteOutcome::Saved
    }
    fn ledger_absent(&self) -> bool {
        true
    }
    fn spec_files(&mut self) -> Vec<String> {
        self.files.clone()
    }
    fn start_generation_for(&mut self, _spec: &str, _focus: &str) -> Result<Box<dyn GenerationJob>, String> {
        let mut attempts = self.attempts.lock().unwrap();
        *attempts += 1;
        match *attempts {
            1 => Err("the spec file vanished".into()),
            2 => Ok(Box::new(Failing(Some("agent exited 1".into())))),
            _ => Ok(Box::new(Idle)),
        }
    }
}

fn flaky_screen(backend: &mut Flaky, events: Vec<Event>) -> Vec<String> {
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

#[test]
fn a_startup_failure_closes_the_picker_shows_the_error_and_can_be_retried() {
    let attempts = Arc::new(Mutex::new(0));
    let mut backend = Flaky { files: vec!["SPEC.md".into()], attempts: Arc::clone(&attempts) };

    let lines = flaky_screen(&mut backend, vec![key(KeyCode::Char('g')), key(KeyCode::Enter)]);

    assert!(has(&lines, "the spec file vanished"), "{lines:?}");
    assert!(!has(&lines, "Section"), "{lines:?}");

    let lines = flaky_screen(
        &mut backend,
        vec![key(KeyCode::Char('g')), key(KeyCode::Enter), key(KeyCode::Char('g')), key(KeyCode::Enter)],
    );
    assert_eq!(*attempts.lock().unwrap(), 3);
    assert!(has(&lines, "Generating"), "{lines:?}");
}
