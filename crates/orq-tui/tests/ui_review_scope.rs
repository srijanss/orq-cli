use std::collections::VecDeque;
use std::io;

use orq_tui::{run_browser, BrowserBackend, BrowserEntry, EventSource, LedgerErrorView, PopupModel, PromoteOutcome, ReviewRow};
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
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
        Ok(vec![BrowserEntry { name: "alpha".into(), description: None, status: "pending".into(), depends_on: vec![], test_file: None }])
    }
    fn promote(&mut self, _feature: &str) -> PromoteOutcome {
        PromoteOutcome::Saved
    }
    fn load_reviews(&mut self) -> Result<Vec<ReviewRow>, String> {
        // A different checkout that happens to share this project's display name.
        let row = |feature: &str, current: bool| ReviewRow {
            run_id: format!("run-{feature}"),
            feature: feature.into(),
            project: "orq-cli".into(),
            status: "ready".into(),
            risk: "low".into(),
            current,
            stale: false,
            has_link: false,
        };
        Ok(vec![row("MINE-01", true), row("TWIN-01", false)])
    }
}

fn screen(chars: &str) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let mut events = vec![Event::Key(KeyEvent::new(KeyCode::Char('V'), KeyModifiers::NONE))];
    events.extend(chars.chars().map(|c| Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))));
    let _ = run_browser(&mut terminal, &model, &mut Fake, &mut Script(events.into()));
    let buffer = terminal.backend().buffer();
    (0..24u16)
        .map(|y| (0..80u16).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
        .collect()
}

fn has(lines: &[String], needle: &str) -> bool {
    lines.iter().any(|l| l.contains(needle))
}

#[test]
fn a_project_that_shares_the_display_name_is_not_in_the_current_scope() {
    let scoped = screen("");
    assert!(has(&scoped, "MINE-01") && !has(&scoped, "TWIN-01"), "{scoped:?}");
    let all = screen("a");
    assert!(has(&all, "MINE-01") && has(&all, "TWIN-01"), "{all:?}");
}
