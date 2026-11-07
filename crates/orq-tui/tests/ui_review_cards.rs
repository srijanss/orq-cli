use std::collections::VecDeque;
use std::io;

use orq_tui::{run_browser, BrowserBackend, BrowserEntry, EventSource, LedgerErrorView, PopupModel, PromoteOutcome, ReviewRow};
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Terminal;

struct Script(VecDeque<Event>);

impl EventSource for Script {
    fn next_event(&mut self) -> io::Result<Event> {
        self.0.pop_front().ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "script exhausted"))
    }
}

struct Fake(Vec<ReviewRow>);

impl BrowserBackend for Fake {
    fn load(&mut self) -> Result<Vec<BrowserEntry>, LedgerErrorView> {
        Ok(vec![])
    }
    fn promote(&mut self, _feature: &str) -> PromoteOutcome {
        PromoteOutcome::Saved
    }
    fn load_reviews(&mut self) -> Result<Vec<ReviewRow>, String> {
        Ok(self.0.clone())
    }
}

fn reviews(count: usize) -> Vec<ReviewRow> {
    (1..=count)
        .map(|n| ReviewRow {
            run_id: format!("run-{n}"),
            feature: format!("COM-{n:02}"),
            project: "orq-cli".into(),
            status: "ready".into(),
            risk: "low".into(),
            current: true,
            stale: false,
            has_link: false,
        })
        .collect()
}

fn click(column: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column, row, modifiers: KeyModifiers::NONE })
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

/// The Reviews tab (opened with a click on its tab) after `events`, as one string per screen row.
fn screen(height: u16, count: usize, events: Vec<Event>) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(TestBackend::new(80, height)).unwrap();
    let mut script = vec![click(44, 2)];
    script.extend(events);
    let _ = run_browser(&mut terminal, &model, &mut Fake(reviews(count)), &mut Script(script.into()));
    let buffer = terminal.backend().buffer();
    (0..height).map(|y| (0..80u16).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string()).collect()
}

fn selected(lines: &[String]) -> Vec<String> {
    lines.iter().filter_map(|l| l.find("> COM-").map(|at| l[at + 2..at + 8].to_string())).collect()
}

fn row_of(lines: &[String], needle: &str) -> u16 {
    u16::try_from(lines.iter().position(|l| l.contains(needle)).unwrap_or_else(|| panic!("{needle} is shown: {lines:?}"))).unwrap()
}

#[test]
fn a_press_on_any_line_of_a_card_selects_that_review() {
    let before = screen(24, 6, vec![]);
    let top = row_of(&before, "COM-03") - 1;
    for line in 0..4 {
        let lines = screen(24, 6, vec![click(10, top + line)]);
        assert_eq!(selected(&lines), ["COM-03"], "line {line} of the card: {lines:?}");
    }
}

#[test]
fn moving_past_the_last_visible_card_scrolls_so_the_selection_stays_on_screen() {
    let lines = screen(24, 6, vec![key(KeyCode::Down); 5]);
    assert_eq!(selected(&lines), ["COM-06"], "{lines:?}");
    assert!(!lines.iter().any(|l| l.contains("COM-01")), "{lines:?}");
}

#[test]
fn a_press_after_scrolling_selects_the_card_under_it() {
    let scrolled = screen(24, 6, vec![key(KeyCode::Down); 5]);
    let top = row_of(&scrolled, "COM-05") - 1;
    let lines = screen(24, 6, [vec![key(KeyCode::Down); 5], vec![click(10, top + 1)]].concat());
    assert_eq!(selected(&lines), ["COM-05"], "{lines:?}");
}

#[test]
fn a_press_below_the_last_card_or_on_the_footer_changes_nothing() {
    let before = screen(24, 6, vec![]);
    let last_border = before.iter().rposition(|l| l.contains('╰')).unwrap();
    let footer = before.len() - 1;
    for y in last_border + 1..=footer {
        let lines = screen(24, 6, vec![click(10, u16::try_from(y).unwrap())]);
        assert_eq!(selected(&lines), ["COM-01"], "row {y}: {lines:?}");
    }
}
