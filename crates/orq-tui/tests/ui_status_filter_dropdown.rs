use std::collections::VecDeque;
use std::io;

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

struct Fixed(Vec<BrowserEntry>);

impl BrowserBackend for Fixed {
    fn load(&mut self) -> Result<Vec<BrowserEntry>, LedgerErrorView> {
        Ok(self.0.clone())
    }
    fn promote(&mut self, _feature: &str) -> PromoteOutcome {
        PromoteOutcome::Saved
    }
}

fn entry(name: &str, status: &str) -> BrowserEntry {
    BrowserEntry {
        name: name.into(),
        description: Some(format!("{name} details")),
        status: status.into(),
        depends_on: vec![],
        test_file: None,
    }
}

fn ledger() -> Vec<BrowserEntry> {
    vec![
        entry("alpha", "draft"),
        entry("bravo", "pending"),
        entry("candy", "pending"),
        entry("delta", "in_progress"),
        entry("zulu", "mystery"),
    ]
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn ch(c: char) -> Event {
    key(KeyCode::Char(c))
}

fn click(column: usize, row: usize) -> Event {
    Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: u16::try_from(column).unwrap(),
        row: u16::try_from(row).unwrap(),
        modifiers: KeyModifiers::NONE,
    })
}

/// Runs the script; returns whether the popup closed itself and the last screen.
fn run(entries: Vec<BrowserEntry>, events: Vec<Event>) -> (bool, Vec<String>) {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let (width, height) = (60u16, 24u16);
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    let closed = run_browser(&mut terminal, &model, &mut Fixed(entries), &mut Script(events.into())).is_ok();
    let buffer = terminal.backend().buffer();
    let lines = (0..height)
        .map(|y| (0..width).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
        .collect();
    (closed, lines)
}

fn has(lines: &[String], needle: &str) -> bool {
    lines.iter().any(|l| l.contains(needle))
}

/// Cell position of `needle`, found by character index (every cell is one char here).
fn find(lines: &[String], needle: &str) -> (usize, usize) {
    lines
        .iter()
        .enumerate()
        .find_map(|(row, l)| {
            let byte = l.find(needle)?;
            Some((l[..byte].chars().count(), row))
        })
        .unwrap_or_else(|| panic!("{needle:?} not on screen: {lines:#?}"))
}

#[test]
fn the_control_shows_all_by_default_beside_the_search_area() {
    let (_, lines) = run(ledger(), vec![]);

    assert!(has(&lines, "Status: All ▾"), "{lines:#?}");
}

#[test]
fn f_lists_all_and_every_known_status() {
    let (closed, lines) = run(ledger(), vec![ch('f')]);

    assert!(!closed);
    for option in ["All", "draft", "pending", "in_progress", "completed", "other"] {
        assert!(has(&lines, &format!("  {option}")) || has(&lines, &format!("> {option}")), "{option}: {lines:#?}");
    }
}

#[test]
fn down_and_enter_filter_the_list_and_the_control_names_the_status() {
    let (_, lines) = run(ledger(), vec![ch('f'), key(KeyCode::Down), key(KeyCode::Down), key(KeyCode::Enter)]);

    assert!(has(&lines, "Status: pending ▾"), "{lines:#?}");
    assert!(has(&lines, "bravo") && has(&lines, "candy"), "{lines:#?}");
    assert!(!has(&lines, "alpha") && !has(&lines, "delta") && !has(&lines, "zulu"), "{lines:#?}");
    assert!(!has(&lines, "> All"), "the dropdown closes on selection: {lines:#?}");
}

#[test]
fn escape_closes_the_dropdown_only_and_keeps_the_filter() {
    let (closed, lines) =
        run(ledger(), vec![ch('f'), key(KeyCode::Down), key(KeyCode::Esc)]);

    assert!(!closed, "Esc closes the dropdown, not the popup");
    assert!(has(&lines, "Status: All ▾"), "{lines:#?}");
    assert!(has(&lines, "alpha") && has(&lines, "candy"), "{lines:#?}");
}

#[test]
fn the_status_filter_combines_with_the_name_search() {
    let events = vec![
        ch('f'),
        key(KeyCode::Down),
        key(KeyCode::Down),
        key(KeyCode::Enter),
        ch('/'),
        ch('c'),
        ch('a'),
        key(KeyCode::Enter),
    ];
    let (_, lines) = run(ledger(), events);

    assert!(has(&lines, "candy"), "{lines:#?}");
    assert!(!has(&lines, "bravo") && !has(&lines, "alpha"), "{lines:#?}");
}

#[test]
fn selecting_all_again_resets_the_filter() {
    let events = vec![
        ch('f'),
        key(KeyCode::Down),
        key(KeyCode::Enter),
        ch('f'),
        key(KeyCode::Up),
        key(KeyCode::Enter),
    ];
    let (_, lines) = run(ledger(), events);

    assert!(has(&lines, "Status: All ▾"), "{lines:#?}");
    assert!(has(&lines, "alpha") && has(&lines, "bravo") && has(&lines, "candy"), "{lines:#?}");
}

#[test]
fn other_selects_statuses_the_browser_has_no_name_for() {
    let events = vec![ch('f'), key(KeyCode::Down), key(KeyCode::Down), key(KeyCode::Down), key(KeyCode::Down), key(KeyCode::Down), key(KeyCode::Enter)];
    let (_, lines) = run(ledger(), events);

    assert!(has(&lines, "Status: other ▾"), "{lines:#?}");
    assert!(has(&lines, "zulu") && !has(&lines, "alpha") && !has(&lines, "delta"), "{lines:#?}");
}

#[test]
fn selection_and_open_follow_the_filtered_rows() {
    let events = vec![
        ch('f'),
        key(KeyCode::Down),
        key(KeyCode::Down),
        key(KeyCode::Enter),
        key(KeyCode::Down),
        key(KeyCode::Down),
        key(KeyCode::Enter),
    ];
    let (_, lines) = run(ledger(), events);

    assert!(has(&lines, "candy details"), "the second filtered row opens: {lines:#?}");
}

#[test]
fn clicking_the_control_then_an_option_filters() {
    let (_, screen) = run(ledger(), vec![]);
    let (x, y) = find(&screen, "Status: All");

    // Options sit under the control inside a border: All, draft, pending, ...
    let (_, lines) = run(ledger(), vec![click(x + 2, y), click(x + 2, y + 2 + 2)]);

    assert!(has(&lines, "Status: pending ▾"), "{lines:#?}");
    assert!(has(&lines, "bravo") && !has(&lines, "alpha"), "{lines:#?}");
}

#[test]
fn a_click_elsewhere_closes_the_dropdown_without_filtering() {
    let (_, screen) = run(ledger(), vec![]);
    let (x, y) = find(&screen, "Status: All");

    let (closed, lines) = run(ledger(), vec![click(x + 2, y), click(0, 22)]);

    assert!(!closed);
    assert!(has(&lines, "Status: All ▾") && has(&lines, "alpha"), "{lines:#?}");
    assert!(!has(&lines, "> All"), "{lines:#?}");
}

#[test]
fn f_while_typing_a_search_is_text_not_a_dropdown() {
    let (_, lines) = run(ledger(), vec![ch('/'), ch('f')]);

    assert!(has(&lines, "/f"), "{lines:#?}");
    assert!(!has(&lines, "> All"), "{lines:#?}");
}
