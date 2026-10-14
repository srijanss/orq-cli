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

fn entry(name: &str, status: &str) -> BrowserEntry {
    BrowserEntry {
        name: name.into(),
        description: Some(format!("{name} description")),
        status: status.into(),
        depends_on: vec![],
        test_file: None,
    }
}

fn press(keys: &[KeyCode]) -> String {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let entries = vec![entry("aaa", "pending"), entry("bbb", "pending"), entry("ccc", "completed"), entry("ddd", "completed")];
    let mut terminal = Terminal::new(TestBackend::new(60, 20)).unwrap();
    let mut events: Vec<Event> =
        keys.iter().map(|k| Event::Key(KeyEvent::new(*k, KeyModifiers::NONE))).collect();
    events.push(Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)));
    let _ = run_browser(&mut terminal, &model, &mut Fixed(entries), &mut Script(events.into()));
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|y| (0..buffer.area.width).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

fn typed(query: &str) -> Vec<KeyCode> {
    let mut keys = vec![KeyCode::Char('/')];
    keys.extend(query.chars().map(KeyCode::Char));
    keys.push(KeyCode::Enter);
    keys
}

#[test]
fn a_status_only_query_opens_the_displayed_feature_not_the_first_underlying_row() {
    let mut keys = typed("completed");
    keys.extend([KeyCode::Down, KeyCode::Down, KeyCode::Enter]);
    let screen = press(&keys);

    assert!(screen.contains("ddd description"), "{screen}");
    assert!(!screen.contains("aaa description") && !screen.contains("ccc description"), "{screen}");
}

#[test]
fn a_query_that_matches_nothing_cannot_open_a_feature() {
    let mut keys = typed("blocked");
    keys.extend([KeyCode::Down, KeyCode::Enter]);
    let screen = press(&keys);

    assert!(screen.contains("no matches"), "{screen}");
    assert!(!screen.contains("description"), "{screen}");
}
