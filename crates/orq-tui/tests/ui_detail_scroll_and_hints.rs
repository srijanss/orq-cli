use std::collections::VecDeque;
use std::io;

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

struct Fixed(Vec<BrowserEntry>);

impl BrowserBackend for Fixed {
    fn load(&mut self) -> Result<Vec<BrowserEntry>, LedgerErrorView> {
        Ok(self.0.clone())
    }
    fn promote(&mut self, _feature: &str) -> PromoteOutcome {
        PromoteOutcome::Saved
    }
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

/// The bottom border row after opening the detail of a feature whose
/// description is `description`, then pressing `keys`.
fn bottom_row(description: &str, keys: Vec<KeyCode>, height: u16) -> String {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let entry = BrowserEntry {
        name: "alpha".into(),
        description: Some(description.into()),
        status: "pending".into(),
        depends_on: vec![],
        test_file: None,
    };
    let mut terminal = Terminal::new(TestBackend::new(60, height)).unwrap();
    let mut events = vec![key(KeyCode::Down), key(KeyCode::Enter)];
    events.extend(keys.into_iter().map(key));
    events.push(key(KeyCode::Char('x')));
    let _ = run_browser(&mut terminal, &model, &mut Fixed(vec![entry]), &mut Script(events.into()));
    let buffer = terminal.backend().buffer();
    let rows: Vec<String> = (0..buffer.area.height)
        .map(|y| (0..buffer.area.width).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>())
        .collect();
    rows.iter().rev().find(|row| row.contains('╯') && row.contains("Esc")).cloned().unwrap_or_else(|| rows.join("\n"))
}

#[test]
fn a_detail_longer_than_the_panel_shows_which_lines_are_visible_and_the_range_moves_when_scrolled() {
    let long = "word ".repeat(120);
    let top = bottom_row(&long, vec![], 14);
    let scrolled = bottom_row(&long, vec![KeyCode::PageDown], 14);

    assert!(top.contains("1-") && top.contains('/'), "{top}");
    assert_ne!(top, scrolled, "the indicator did not change");
    assert!(scrolled.contains('/') && !scrolled.contains(" 1-"), "{scrolled}");
}

#[test]
fn a_detail_that_fits_shows_no_position_indicator() {
    let row = bottom_row("short", vec![], 30);

    assert!(row.contains("Esc") && !row.contains('/'), "{row}");
}
