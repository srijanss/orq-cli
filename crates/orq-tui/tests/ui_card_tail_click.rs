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
    BrowserEntry { name: name.into(), description: None, status: status.into(), depends_on: vec![], test_file: None }
}

fn click(column: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column, row, modifiers: KeyModifiers::NONE })
}

fn frame_lines(terminal: &Terminal<TestBackend>) -> Vec<String> {
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|y| (0..buffer.area.width).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
        .collect()
}

/// 40x18: chrome rows 0-4, project rows 5-6, list rows 7..=17 (eleven rows):
/// two whole four-row cards at rows 7-10 and 11-14, then three unused rows.
fn run(events: Vec<Event>) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let entries = vec![entry("alpha", "completed"), entry("beta", "draft"), entry("gamma", "pending")];
    let mut terminal = Terminal::new(TestBackend::new(40, 18)).unwrap();
    let mut events = events;
    events.push(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
    run_browser(&mut terminal, &model, &mut Fixed(entries), &mut Script(events.into())).unwrap();
    frame_lines(&terminal)
}

#[test]
fn a_click_in_the_unused_rows_below_the_last_whole_card_selects_nothing() {
    for row in [15, 16, 17] {
        let lines = run(vec![click(5, row)]);
        assert!(!lines.iter().any(|l| l.contains("> ")), "row {row}: {lines:?}");
    }
}

#[test]
fn a_click_on_a_rendered_card_still_selects_it() {
    let lines = run(vec![click(5, 12)]);
    assert!(lines.iter().any(|l| l.contains("> beta")), "{lines:?}");
}
