use std::cell::RefCell;
use std::collections::VecDeque;
use std::io;
use std::rc::Rc;

use orq_tui::{run_browser, BrowserBackend, BrowserEntry, EventSource, LedgerErrorView, PopupModel, PromoteOutcome};
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
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
    fail_for: Vec<String>,
    fail_load_at: Option<usize>,
}

struct Fake(Rc<RefCell<Shared>>);

impl BrowserBackend for Fake {
    fn load(&mut self) -> Result<Vec<BrowserEntry>, LedgerErrorView> {
        let mut shared = self.0.borrow_mut();
        shared.loads += 1;
        if shared.fail_load_at == Some(shared.loads) {
            return Err(LedgerErrorView { path: "/repo/.tdd-features.json".into(), reason: "reload boom".into() });
        }
        if shared.loads == 2 {
            if let Some(changed) = shared.on_second_load.take() {
                shared.entries = changed;
            }
        }
        Ok(shared.entries.clone())
    }

    fn promote(&mut self, feature: &str) -> PromoteOutcome {
        let mut shared = self.0.borrow_mut();
        shared.promotes.push(feature.to_string());
        if shared.fail_for.iter().any(|f| f == feature) {
            return PromoteOutcome::Failed { reason: "disk full".into() };
        }
        for e in shared.entries.iter_mut().filter(|e| e.name == feature) {
            e.status = "pending".into();
        }
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

#[allow(dead_code)]
fn click(column: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

fn space() -> Event {
    key(KeyCode::Char(' '))
}

fn down() -> Event {
    key(KeyCode::Down)
}

/// Marks the rows at the given 1-based positions in a list of `total` rows.
fn mark(positions: &[usize], total: usize) -> Vec<Event> {
    let mut events = Vec::new();
    let mut at = 0;
    for &position in positions {
        while at < position {
            events.push(down());
            at += 1;
        }
        events.push(space());
    }
    let _ = total;
    events
}

#[test]
fn the_bulk_button_stays_while_the_filter_leaves_nothing_selected() {
    let state = shared(vec![entry("alpha", "draft"), entry("beta", "pending")]);
    let mut events = vec![down(), space(), key(KeyCode::Char('f')), down(), key(KeyCode::Enter)];
    events.push(key(KeyCode::Char('r')));
    let (_, lines) = run(&state, 60, 24, events);

    assert!(has(&lines, "Promote 1 to pending"), "{lines:#?}");
}

#[test]
fn p_promotes_the_selected_draft_when_only_non_drafts_are_marked() {
    let state = shared(vec![entry("alpha", "draft"), entry("beta", "completed")]);
    let events = vec![down(), down(), space(), key(KeyCode::Up), key(KeyCode::Char('p'))];

    let (_, lines) = run(&state, 60, 24, events);

    assert_eq!(state.borrow().promotes, vec!["alpha".to_string()], "{lines:#?}");
}

#[test]
fn f_does_not_open_an_invisible_dropdown_on_a_narrow_viewport() {
    let state = shared(vec![entry("alpha", "draft"), entry("beta", "pending")]);
    // Esc closes the popup only if no dropdown swallowed the first press.
    let events = vec![key(KeyCode::Char('f')), key(KeyCode::Esc)];

    let (closed, lines) = run(&state, 24, 24, events);

    assert!(closed, "{lines:#?}");
}

#[test]
fn a_failed_reload_after_a_bulk_promote_keeps_the_outcome_and_the_failed_mark() {
    let state = shared(vec![entry("alpha", "draft"), entry("beta", "draft"), entry("gamma", "draft")]);
    state.borrow_mut().fail_for = vec!["beta".to_string()];
    state.borrow_mut().fail_load_at = Some(2);
    let mut events = mark(&[1, 2, 3], 3);
    events.push(key(KeyCode::Char('p')));

    let (_, lines) = run(&state, 60, 24, events.clone());
    assert!(has(&lines, "reload boom"), "{lines:#?}");
    assert!(has(&lines, "saved 2"), "{lines:#?}");
    assert!(has(&lines, "beta: disk full"), "{lines:#?}");
    assert_eq!(state.borrow().promotes.len(), 3);

    // Recover with r, then p again: only the failed beta is retried, unmarked by hand.
    let state = shared(vec![entry("alpha", "draft"), entry("beta", "draft"), entry("gamma", "draft")]);
    state.borrow_mut().fail_for = vec!["beta".to_string()];
    state.borrow_mut().fail_load_at = Some(2);
    events.push(key(KeyCode::Char('r')));
    events.push(key(KeyCode::Char('p')));
    let (_, lines) = run(&state, 60, 24, events);
    let promotes = state.borrow().promotes.clone();
    assert_eq!(promotes.len(), 4, "{promotes:?} {lines:#?}");
    assert_eq!(promotes[3], "beta");
}
