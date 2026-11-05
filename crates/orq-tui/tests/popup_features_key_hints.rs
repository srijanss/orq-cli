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

/// The screen after pressing `keys` on a Features tab holding one feature.
fn screen(width: u16, height: u16, keys: Vec<KeyCode>) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let entry = BrowserEntry { name: "alpha".into(), description: None, status: "pending".into(), depends_on: vec![], test_file: None };
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    let mut events: Vec<Event> = keys.into_iter().map(key).collect();
    events.push(key(KeyCode::Esc));
    let _ = run_browser(&mut terminal, &model, &mut Fixed(vec![entry]), &mut Script(events.into()));
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|y| (0..buffer.area.width).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>())
        .collect()
}

#[test]
fn the_features_tab_footer_lists_every_key() {
    let rows = screen(170, 24, vec![KeyCode::Down]);
    let footer = rows.last().unwrap();
    for hint in ["a add", "e edit", "f status", "Space mark", "p promote", "L launch", "V review", "R resume", "/ search", "Tab settings"] {
        assert!(footer.contains(hint), "{hint}: {footer:?}");
    }
    let narrow = screen(40, 24, vec![KeyCode::Down]);
    assert!(narrow.last().unwrap().contains("↑↓ move") && narrow.iter().all(|r| r.chars().count() <= 40));
}
