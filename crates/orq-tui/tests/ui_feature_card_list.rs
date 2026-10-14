use std::collections::VecDeque;
use std::io;

use orq_tui::{run_browser, BrowserBackend, BrowserEntry, EventSource, LedgerErrorView, PopupModel, PromoteOutcome};
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::style::Color;
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

fn entry(name: &str, status: &str, depends_on: &[&str]) -> BrowserEntry {
    BrowserEntry {
        name: name.into(),
        description: None,
        status: status.into(),
        depends_on: depends_on.iter().map(|d| d.to_string()).collect(),
        test_file: None,
    }
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn click(column: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column, row, modifiers: KeyModifiers::NONE })
}

fn wheel_down(column: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent { kind: MouseEventKind::ScrollDown, column, row, modifiers: KeyModifiers::NONE })
}

/// Runs the browser at 40x18 (two cards fit) through `events`, then Esc.
fn run(entries: Vec<BrowserEntry>, mut events: Vec<Event>) -> Terminal<TestBackend> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(TestBackend::new(40, 18)).unwrap();
    events.push(key(KeyCode::Esc));
    run_browser(&mut terminal, &model, &mut Fixed(entries), &mut Script(events.into())).unwrap();
    terminal
}

fn lines(terminal: &Terminal<TestBackend>) -> Vec<String> {
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|y| (0..buffer.area.width).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
        .collect()
}

fn three() -> Vec<BrowserEntry> {
    vec![
        entry("alpha", "completed", &[]),
        entry("beta", "draft", &["alpha"]),
        entry("gamma", "pending", &["alpha", "beta"]),
    ]
}

// Rows: 0-4 chrome, 5-6 project header, 7.. cards of four rows each.

#[test]
fn each_feature_is_a_rounded_card_with_a_dependency_line_the_name_and_the_status_at_the_right() {
    let terminal = run(three(), vec![]);
    let lines = lines(&terminal);

    assert!(lines[7].starts_with('╭') && lines[7].ends_with('╮'), "{lines:?}");
    assert!(lines[8].starts_with('│') && lines[8].contains("no dependencies"), "{lines:?}");
    assert!(lines[9].contains("alpha") && lines[9].ends_with("completed│"), "{lines:?}");
    assert!(lines[10].starts_with('╰'), "{lines:?}");
    assert!(lines[12].contains("depends on alpha"), "{lines:?}");
    assert!(lines[13].contains("beta") && lines[13].ends_with("draft│"), "{lines:?}");
    // Only whole cards are drawn: the third does not fit.
    assert!(!lines.iter().any(|l| l.contains("gamma")), "{lines:?}");
}

#[test]
fn the_status_badge_keeps_its_colour_and_the_selected_card_has_a_brighter_border_and_a_marker() {
    let terminal = run(three(), vec![key(KeyCode::Down), key(KeyCode::Down)]);
    let buffer = terminal.backend().buffer();
    let lines = lines(&terminal);

    // After two Downs the second card (beta) is selected.
    assert_eq!(buffer[(0, 7)].fg, Color::DarkGray, "{lines:?}");
    assert_eq!(buffer[(0, 11)].fg, Color::White, "{lines:?}");
    assert!(lines[13].contains("> beta"), "{lines:?}");
    assert!(!lines[9].contains("> "), "{lines:?}");
    let status_at = u16::try_from(lines[9].find("completed").unwrap()).unwrap();
    assert_eq!(buffer[(status_at, 9)].fg, Color::Green);
}

#[test]
fn moving_past_the_last_visible_card_scrolls_by_whole_cards() {
    let terminal = run(three(), vec![key(KeyCode::Down), key(KeyCode::Down), key(KeyCode::Down)]);
    let lines = lines(&terminal);

    assert!(lines.iter().any(|l| l.contains("> gamma")), "{lines:?}");
    assert!(!lines.iter().any(|l| l.contains("completed")), "{lines:?}");
}

#[test]
fn a_click_anywhere_on_a_card_selects_it_and_the_wheel_scrolls_one_card() {
    let clicked = lines(&run(three(), vec![click(5, 12)]));
    assert!(clicked.iter().any(|l| l.contains("> beta")), "{clicked:?}");

    let wheeled = lines(&run(three(), vec![wheel_down(5, 8)]));
    assert!(wheeled.iter().any(|l| l.contains("gamma")), "{wheeled:?}");
    assert!(!wheeled.iter().any(|l| l.contains("completed")), "{wheeled:?}");
}
