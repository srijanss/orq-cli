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

struct Fake;

impl BrowserBackend for Fake {
    fn load(&mut self) -> Result<Vec<BrowserEntry>, LedgerErrorView> {
        let entry = |name: &str, status: &str| BrowserEntry {
            name: name.into(),
            description: None,
            status: status.into(),
            depends_on: vec![],
            test_file: None,
        };
        Ok(vec![entry("alpha", "pending"), entry("beta", "draft"), entry("gamma", "done")])
    }
    fn promote(&mut self, _feature: &str) -> PromoteOutcome {
        PromoteOutcome::Saved
    }
    fn launch_profiles(&mut self) -> Result<Vec<String>, String> {
        Ok(vec!["opus".into(), "sonnet".into()])
    }
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn click(column: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column, row, modifiers: KeyModifiers::NONE })
}

/// The rows after the events; the loop ends when the script runs out.
fn screen(events: Vec<Event>) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    let _ = run_browser(&mut terminal, &model, &mut Fake, &mut Script(VecDeque::from(events)));
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|y| (0..buffer.area.width).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>())
        .collect()
}

fn find(rows: &[String], needle: &str) -> Option<(u16, u16)> {
    rows.iter().enumerate().find_map(|(y, r)| r.find(needle).map(|byte| (r[..byte].chars().count() as u16, y as u16)))
}

#[test]
fn a_pending_feature_shows_an_orchestrate_button_and_other_statuses_do_not() {
    let pending = screen(vec![key(KeyCode::Down)]);
    assert!(find(&pending, "[ Orchestrate (L) ]").is_some(), "{pending:?}");
    let draft = screen(vec![key(KeyCode::Down), key(KeyCode::Down)]);
    assert!(find(&draft, "Orchestrate (L)").is_none() && find(&draft, "[ Mark pending (p) ]").is_some(), "{draft:?}");
    let done = screen(vec![key(KeyCode::Down), key(KeyCode::Down), key(KeyCode::Down)]);
    assert!(find(&done, "Orchestrate (L)").is_none(), "{done:?}");
}

#[test]
fn clicking_the_orchestrate_button_opens_the_launch_picker() {
    let (x, y) = find(&screen(vec![key(KeyCode::Down)]), "[ Orchestrate (L) ]").unwrap();
    let after = screen(vec![key(KeyCode::Down), click(x + 2, y)]);
    assert!(find(&after, "opus").is_some() && find(&after, "sonnet").is_some(), "{after:?}");
    let elsewhere = screen(vec![key(KeyCode::Down), click(x + 40, y)]);
    assert!(find(&elsewhere, "opus").is_none(), "{elsewhere:?}");
}
