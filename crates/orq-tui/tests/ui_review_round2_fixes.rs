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

#[allow(dead_code)]
fn space() -> Event {
    key(KeyCode::Char(' '))
}

fn down() -> Event {
    key(KeyCode::Down)
}

/// Marks the rows at the given 1-based positions in a list of `total` rows.
#[allow(dead_code)]
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

fn six() -> Vec<BrowserEntry> {
    vec![
        entry("alpha", "draft"),
        entry("beta", "pending"),
        entry("gamma", "in_progress"),
        entry("delta", "completed"),
        entry("omega", "weird"),
    ]
}

fn to_last_option() -> Vec<Event> {
    vec![key(KeyCode::Char('f')), down(), down(), down(), down(), down(), key(KeyCode::Enter)]
}

#[test]
fn a_short_viewport_scrolls_the_dropdown_so_every_status_is_selectable() {
    let state = shared(six());

    let (_, lines) = run(&state, 60, 7, to_last_option());

    assert!(has(&lines, "Status: weird"), "{lines:#?}");
}

#[test]
fn a_notice_row_does_not_make_the_dropdown_select_hidden_options() {
    let state = shared(six());
    let mut events = vec![down(), key(KeyCode::Char('p'))];
    events.extend(to_last_option());

    let (_, lines) = run(&state, 60, 7, events);

    assert!(has(&lines, "Status: weird"), "{lines:#?}");
}
