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
fn p_promotes_every_marked_draft_in_list_order_and_clears_their_marks() {
    let state = shared(vec![entry("alpha", "draft"), entry("beta", "draft"), entry("gamma", "draft")]);
    let mut events = mark(&[1, 3], 3);
    events.push(key(KeyCode::Char('p')));

    let (closed, lines) = run(&state, 60, 24, events);

    assert!(!closed);
    assert_eq!(state.borrow().promotes, vec!["alpha".to_string(), "gamma".to_string()]);
    assert!(!has(&lines, "[x]"), "{lines:#?}");
    assert!(has(&lines, "saved 2"), "{lines:#?}");
    let beta = lines.iter().find(|l| l.contains("beta")).expect("beta row");
    assert!(beta.contains("draft"), "{lines:#?}");
}

#[test]
fn marked_rows_that_are_not_drafts_are_skipped_and_counted() {
    let state = shared(vec![entry("alpha", "draft"), entry("beta", "completed")]);
    let mut events = mark(&[1, 2], 2);
    events.push(key(KeyCode::Char('p')));

    let (_, lines) = run(&state, 60, 24, events);

    assert_eq!(state.borrow().promotes, vec!["alpha".to_string()]);
    assert!(has(&lines, "1 skipped"), "{lines:#?}");
    let beta = lines.iter().find(|l| l.contains("beta")).expect("beta row");
    assert!(beta.contains("completed"), "{lines:#?}");
}

#[test]
fn a_failed_promote_is_reported_and_keeps_its_mark_while_the_others_are_saved() {
    let state = shared(vec![entry("alpha", "draft"), entry("beta", "draft"), entry("gamma", "draft")]);
    state.borrow_mut().fail_for = vec!["beta".into()];
    let mut events = mark(&[1, 2, 3], 3);
    events.push(key(KeyCode::Char('p')));

    let (_, lines) = run(&state, 70, 24, events);

    assert_eq!(state.borrow().promotes.len(), 3, "a failure does not stop the others");
    assert!(has(&lines, "saved 2"), "{lines:#?}");
    assert!(has(&lines, "1 failed"), "{lines:#?}");
    assert!(has(&lines, "beta: disk full"), "{lines:#?}");
    assert!(has(&lines, "[x] beta"), "{lines:#?}");
    assert!(!has(&lines, "[x] alpha") && !has(&lines, "[x] gamma"), "{lines:#?}");
}

#[test]
fn the_button_names_the_draft_count_and_clicking_it_promotes_the_marked_drafts() {
    let make = || shared(vec![entry("alpha", "draft"), entry("beta", "draft"), entry("gamma", "completed")]);
    let events = || mark(&[1, 2, 3], 3);

    let state = make();
    let (_, lines) = run(&state, 60, 24, events());
    let row = lines.iter().position(|l| l.contains("Promote 2 to pending")).expect("button");
    let column = lines[row].chars().position(|c| c == 'P').unwrap();

    let state = make();
    let mut clicked = events();
    clicked.push(click(u16::try_from(column).unwrap(), u16::try_from(row).unwrap()));
    run(&state, 60, 24, clicked);

    assert_eq!(state.borrow().promotes, vec!["alpha".to_string(), "beta".to_string()]);
}

#[test]
fn a_click_elsewhere_does_not_promote_the_marked_rows() {
    let state = shared(vec![entry("alpha", "draft")]);
    let mut events = mark(&[1], 1);
    events.push(click(0, 0));

    run(&state, 60, 24, events);

    assert!(state.borrow().promotes.is_empty());
}

#[test]
fn p_while_typing_a_search_is_text_not_a_promote() {
    let state = shared(vec![entry("alpha", "draft")]);
    let mut events = mark(&[1], 1);
    events.extend([key(KeyCode::Char('/')), key(KeyCode::Char('p'))]);

    run(&state, 60, 24, events);

    assert!(state.borrow().promotes.is_empty());
}
