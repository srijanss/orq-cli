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

struct Recording {
    entries: Vec<BrowserEntry>,
    promoted: Vec<String>,
}

impl BrowserBackend for Recording {
    fn load(&mut self) -> Result<Vec<BrowserEntry>, LedgerErrorView> {
        Ok(self.entries.clone())
    }
    fn promote(&mut self, feature: &str) -> PromoteOutcome {
        self.promoted.push(feature.to_string());
        PromoteOutcome::Saved
    }
}

fn entry(name: &str, status: &str) -> BrowserEntry {
    BrowserEntry { name: name.into(), description: None, status: status.into(), depends_on: vec![], test_file: None }
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn click(column: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column, row, modifiers: KeyModifiers::NONE })
}

fn run(width: u16, height: u16, mut events: Vec<Event>) -> (Vec<String>, Vec<String>) {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut backend = Recording { entries: vec![entry("alpha", "completed"), entry("beta", "draft")], promoted: vec![] };
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    events.push(key(KeyCode::Esc));
    run_browser(&mut terminal, &model, &mut backend, &mut Script(events.into())).unwrap();
    (frame_lines(&terminal), backend.promoted)
}

fn frame_lines(terminal: &Terminal<TestBackend>) -> Vec<String> {
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|y| (0..buffer.area.width).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
        .collect()
}

#[test]
fn a_roomy_viewport_shows_the_selected_feature_panel_at_the_bottom_with_a_key_hint() {
    let (lines, _) = run(60, 30, vec![key(KeyCode::Down)]);

    assert!(lines[27].starts_with('╭') && lines[27].contains("Selected feature · alpha"), "{lines:?}");
    assert!(lines[28].contains("completed") && lines[28].contains("only for drafts"), "{lines:?}");
    assert!(lines[29].starts_with('╰') && lines[29].contains("Enter open"), "{lines:?}");
}

#[test]
fn the_panel_offers_mark_pending_for_a_draft_and_a_click_on_it_promotes() {
    let (lines, promoted) = run(60, 30, vec![key(KeyCode::Down), key(KeyCode::Down)]);
    assert!(lines[27].contains("Selected feature · beta"), "{lines:?}");
    assert!(lines[28].contains("[ Mark pending (p) ]") && lines[28].contains("draft"), "{lines:?}");
    assert!(promoted.is_empty());

    let (_, promoted) = run(60, 30, vec![key(KeyCode::Down), key(KeyCode::Down), click(5, 28)]);
    assert_eq!(promoted, vec!["beta".to_string()]);

    let (_, promoted) = run(60, 30, vec![key(KeyCode::Down), key(KeyCode::Down), key(KeyCode::Char('p'))]);
    assert_eq!(promoted, vec!["beta".to_string()]);
}

#[test]
fn a_small_viewport_has_no_panel_and_keeps_the_bottom_button() {
    let (lines, _) = run(40, 14, vec![key(KeyCode::Down), key(KeyCode::Down)]);

    assert!(!lines.iter().any(|l| l.contains("Selected feature")), "{lines:?}");
    assert!(lines.iter().any(|l| l.contains("[ Mark pending (p) ]")), "{lines:?}");
}
