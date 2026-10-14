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
        description: Some(format!("{name} details")),
        status: status.into(),
        depends_on: vec![],
        test_file: None,
    }
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn run(entries: Vec<BrowserEntry>, events: Vec<Event>) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let (width, height) = (60u16, 24u16);
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    let _ = run_browser(&mut terminal, &model, &mut Fixed(entries), &mut Script(events.into()));
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| (0..width).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
        .collect()
}

fn has(lines: &[String], needle: &str) -> bool {
    lines.iter().any(|l| l.contains(needle))
}

#[test]
fn the_dropdown_lists_every_known_status_even_when_none_has_it() {
    let lines = run(vec![entry("alpha", "completed")], vec![key(KeyCode::Char('f'))]);

    for option in ["All", "draft", "pending", "in_progress", "completed"] {
        assert!(has(&lines, option), "{option} missing: {lines:#?}");
    }
}

#[test]
fn choosing_a_status_nothing_has_shows_an_empty_list() {
    // All -> draft is one Down, then Enter.
    let lines = run(
        vec![entry("alpha", "completed")],
        vec![key(KeyCode::Char('f')), key(KeyCode::Down), key(KeyCode::Enter)],
    );

    assert!(has(&lines, "Status: draft"), "{lines:#?}");
    assert!(!has(&lines, "alpha"), "{lines:#?}");
}
