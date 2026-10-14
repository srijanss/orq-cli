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

/// The frame after opening the detail of `alpha`, as trimmed lines.
fn detail_frame(width: u16, height: u16) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let entry = BrowserEntry {
        name: "alpha".into(),
        description: Some("does the thing".into()),
        status: "pending".into(),
        depends_on: vec![],
        test_file: Some("tests/alpha.rs".into()),
    };
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    let events = vec![key(KeyCode::Down), key(KeyCode::Enter), key(KeyCode::Char('x'))];
    // The script ends by exhausting, which run_browser reports as an error;
    // the last frame drawn is what is inspected.
    let _ = run_browser(&mut terminal, &model, &mut Fixed(vec![entry]), &mut Script(events.into()));
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|y| (0..buffer.area.width).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
        .collect()
}

#[test]
fn the_detail_is_drawn_in_a_rounded_panel_titled_with_the_feature_and_a_key_hint() {
    let lines = detail_frame(70, 16);
    let text = lines.join("\n");

    let title = lines.iter().position(|l| l.contains('╭') && l.contains("alpha")).unwrap_or_else(|| panic!("{text}"));
    let bottom = lines.iter().rposition(|l| l.contains('╯') && l.contains("Esc")).unwrap_or_else(|| panic!("{text}"));
    assert!(bottom > title, "{text}");
    // Content sits between the borders.
    assert!(lines[title + 1..bottom].iter().all(|l| l.starts_with('│') && l.ends_with('│')), "{text}");
    assert!(text.contains("does the thing") && text.contains("pending"), "{text}");
}

#[test]
fn a_narrow_terminal_keeps_every_line_inside_the_panel() {
    let lines = detail_frame(24, 12);
    let text = lines.join("\n");

    assert!(lines.iter().all(|l| l.chars().count() <= 24), "{text}");
    assert!(text.contains('╭') && text.contains('╯'), "{text}");
}
