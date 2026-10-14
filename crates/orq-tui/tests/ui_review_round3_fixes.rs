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

fn click(column: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

fn down() -> Event {
    key(KeyCode::Down)
}

fn five() -> Vec<BrowserEntry> {
    vec![
        entry("alpha", "draft"),
        entry("beta", "pending"),
        entry("gamma", "in_progress"),
        entry("delta", "completed"),
        entry("omega", "weird"),
    ]
}

fn open_and_go_down(opener: Event) -> Vec<Event> {
    vec![opener, down(), down(), down(), down(), down()]
}

/// Where the status control is drawn: (column, row) of "Status: All".
fn control_at(height: u16) -> Option<(u16, u16)> {
    let (_, lines) = run(&shared(five()), 60, height, vec![]);
    lines.iter().enumerate().find_map(|(y, l)| {
        let byte = l.find("Status: All")?;
        let column = l[..byte].chars().count();
        Some((u16::try_from(column).ok()?, u16::try_from(y).ok()?))
    })
}

#[test]
fn the_highlighted_last_option_is_drawn_before_it_is_chosen() {
    let state = shared(five());

    let (_, lines) = run(&state, 60, 7, open_and_go_down(key(KeyCode::Char('f'))));

    assert!(has(&lines, "other"), "{lines:#?}");
    assert!(has(&lines, "Status: All"), "nothing is chosen before Enter: {lines:#?}");
}

#[test]
fn the_highlighted_last_option_is_drawn_when_a_notice_takes_a_row() {
    let state = shared(five());
    let mut events = vec![down(), key(KeyCode::Char('p'))];
    events.extend(open_and_go_down(key(KeyCode::Char('f'))));

    let (_, lines) = run(&state, 60, 7, events);

    assert!(has(&lines, "other"), "{lines:#?}");
}

#[test]
fn clicking_a_scrolled_option_chooses_the_one_that_is_drawn() {
    let (column, row) = control_at(7).expect("control drawn at 60x7");
    let (_, lines) = run(&shared(five()), 60, 7, open_and_go_down(key(KeyCode::Char('f'))));
    let (line_y, line) = lines
        .iter()
        .enumerate()
        .find(|(y, l)| *y != usize::from(row) && l.contains("other"))
        .expect("last option drawn");
    let option_column = u16::try_from(line[..line.find("other").unwrap()].chars().count()).unwrap();
    let _ = column;

    let mut events = open_and_go_down(key(KeyCode::Char('f')));
    events.push(click(option_column, u16::try_from(line_y).unwrap()));
    let (_, lines) = run(&shared(five()), 60, 7, events);

    assert!(has(&lines, "Status: other"), "{lines:#?}");
}

#[test]
fn no_path_selects_an_option_that_is_not_drawn() {
    for height in 3..=12u16 {
        for use_mouse in [false, true] {
            let opener = match (use_mouse, control_at(height)) {
                (true, Some((column, row))) => click(column + 1, row),
                (true, None) => continue,
                (false, _) => key(KeyCode::Char('f')),
            };
            let (_, before) = run(&shared(five()), 60, height, open_and_go_down(opener.clone()));
            let mut events = open_and_go_down(opener);
            events.push(key(KeyCode::Enter));
            let (_, after) = run(&shared(five()), 60, height, events);

            if !has(&before, "other") {
                assert!(
                    !has(&after, "Status: other"),
                    "height {height} mouse {use_mouse}: chose an undrawn option\n{before:#?}\n{after:#?}"
                );
            }
        }
    }
}
