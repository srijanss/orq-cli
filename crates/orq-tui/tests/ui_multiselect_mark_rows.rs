use std::cell::RefCell;
use std::collections::VecDeque;
use std::io;
use std::rc::Rc;

use orq_tui::{run_browser, BrowserBackend, BrowserEntry, EventSource, LedgerErrorView, PopupModel, PromoteOutcome};
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;

struct Script(VecDeque<Event>);

impl EventSource for Script {
    fn next_event(&mut self) -> io::Result<Event> {
        self.0.pop_front().ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "script exhausted"))
    }
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn entry(name: &str, status: &str) -> BrowserEntry {
    BrowserEntry { name: name.into(), description: None, status: status.into(), depends_on: vec![], test_file: None }
}

#[derive(Default)]
struct Shared {
    entries: Vec<BrowserEntry>,
    promotes: Vec<String>,
    loads: usize,
    on_second_load: Option<Vec<BrowserEntry>>,
}

struct Fake(Rc<RefCell<Shared>>);

impl BrowserBackend for Fake {
    fn load(&mut self) -> Result<Vec<BrowserEntry>, LedgerErrorView> {
        let mut shared = self.0.borrow_mut();
        shared.loads += 1;
        if shared.loads == 2 {
            if let Some(changed) = shared.on_second_load.take() {
                shared.entries = changed;
            }
        }
        Ok(shared.entries.clone())
    }

    fn promote(&mut self, feature: &str) -> PromoteOutcome {
        self.0.borrow_mut().promotes.push(feature.to_string());
        PromoteOutcome::Saved
    }
}

/// Runs the script and returns whether the popup closed itself, and the last
/// screen. A script that runs out leaves the popup open (`closed == false`).
fn run(state: &Rc<RefCell<Shared>>, width: u16, height: u16, events: Vec<Event>) -> (bool, Vec<String>) {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    let mut events = Script(events.into());
    let closed = run_browser(&mut terminal, &model, &mut Fake(state.clone()), &mut events).is_ok();
    let buffer = terminal.backend().buffer();
    let lines = (0..height)
        .map(|y| (0..width).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
        .collect();
    (closed, lines)
}

fn shared(entries: Vec<BrowserEntry>) -> Rc<RefCell<Shared>> {
    Rc::new(RefCell::new(Shared { entries, ..Default::default() }))
}

fn has(lines: &[String], needle: &str) -> bool {
    lines.iter().any(|l| l.contains(needle))
}

#[test]
fn space_marks_the_highlighted_row_and_toggles_it_off_again() {
    let state = shared(vec![entry("alpha", "draft"), entry("beta", "draft")]);

    let (_, lines) = run(&state, 50, 24, vec![key(KeyCode::Down), key(KeyCode::Char(' '))]);
    assert!(has(&lines, "[x] alpha"), "{lines:#?}");
    assert!(has(&lines, "[ ] beta"), "{lines:#?}");

    let (_, lines) = run(&state, 50, 24, vec![key(KeyCode::Down), key(KeyCode::Char(' ')), key(KeyCode::Char(' '))]);
    assert!(!has(&lines, "[x]"), "{lines:#?}");
    assert!(state.borrow().promotes.is_empty(), "marking never writes the ledger");
}

#[test]
fn marks_survive_moving_and_searching() {
    let state = shared(vec![entry("alpha", "draft"), entry("beta", "draft")]);
    let mut events = vec![key(KeyCode::Down), key(KeyCode::Char(' ')), key(KeyCode::Down)];
    events.extend([key(KeyCode::Char('/')), key(KeyCode::Char('b')), key(KeyCode::Enter)]);
    // Clearing the search is Esc's first job; the mark stays.
    events.push(key(KeyCode::Esc));

    let (closed, lines) = run(&state, 50, 24, events);

    assert!(!closed);
    assert!(has(&lines, "[x] alpha"), "{lines:#?}");
}

#[test]
fn escape_clears_the_marks_before_it_closes_the_popup() {
    let state = shared(vec![entry("alpha", "draft")]);

    let (closed, lines) = run(&state, 50, 24, vec![key(KeyCode::Down), key(KeyCode::Char(' ')), key(KeyCode::Esc)]);
    assert!(!closed, "the first Esc only clears the marks");
    assert!(!has(&lines, "[x]"), "{lines:#?}");

    let (closed, _) = run(
        &state,
        50,
        24,
        vec![key(KeyCode::Down), key(KeyCode::Char(' ')), key(KeyCode::Esc), key(KeyCode::Esc)],
    );
    assert!(closed);
}

#[test]
fn marked_count_is_shown_in_the_hint() {
    let state = shared(vec![entry("alpha", "draft"), entry("beta", "draft")]);

    let (_, lines) = run(
        &state,
        60,
        24,
        vec![key(KeyCode::Down), key(KeyCode::Char(' ')), key(KeyCode::Down), key(KeyCode::Char(' '))],
    );

    assert!(has(&lines, "2 marked"), "{lines:#?}");
}

#[test]
fn a_mark_is_dropped_when_its_feature_is_gone_after_a_reload() {
    let state = shared(vec![entry("alpha", "draft"), entry("gone", "draft")]);
    state.borrow_mut().on_second_load = Some(vec![entry("alpha", "draft"), entry("gone2", "draft")]);

    let (_, lines) = run(
        &state,
        60,
        24,
        vec![key(KeyCode::Down), key(KeyCode::Down), key(KeyCode::Char(' ')), key(KeyCode::Char('r'))],
    );

    assert!(!has(&lines, "[x]"), "{lines:#?}");
    assert!(!has(&lines, "1 marked"), "{lines:#?}");
}
