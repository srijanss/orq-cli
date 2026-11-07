use std::collections::VecDeque;
use std::io;

use orq_tui::{run_browser, BrowserBackend, BrowserEntry, EventSource, LedgerErrorView, PopupModel, PromoteOutcome, RunRow};
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
        Ok(vec![BrowserEntry {
            name: "alpha".into(),
            description: None,
            status: "pending".into(),
            depends_on: vec![],
            test_file: None,
        }])
    }
    fn promote(&mut self, _feature: &str) -> PromoteOutcome {
        PromoteOutcome::Saved
    }
    fn load_runs(&mut self) -> Result<Vec<RunRow>, String> {
        Ok(vec![RunRow {
            run_id: "run-1".into(),
            feature: "COM-01".into(),
            status: "running".into(),
            stage: None,
            implementer_model: None,
            reviewer_model: None,
            has_review_link: false,
            reason: None,
        }])
    }
}

fn click(column: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

fn show(events: Vec<Event>) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
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
fn the_tab_bar_lists_orchestrations_after_features_and_settings() {
    let lines = show(vec![]);

    let bar = &lines[2];
    let (features, settings, runs) = (bar.find("Features"), bar.find("Settings"), bar.find("Orchestrations"));
    assert!(features < settings && settings < runs && runs.is_some(), "{lines:?}");
}

#[test]
fn clicking_the_runs_tab_opens_the_runs_view_from_features_and_from_settings() {
    let from_features = show(vec![click(26, 2)]);
    assert!(has(&from_features, "COM-01"), "{from_features:?}");
    assert!(!has(&from_features, "alpha"), "{from_features:?}");

    let from_settings = show(vec![Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)), click(26, 2)]);
    assert!(has(&from_settings, "COM-01"), "{from_settings:?}");
}

#[test]
fn clicking_the_features_tab_leaves_the_runs_view() {
    let lines = show(vec![click(26, 2), click(2, 2)]);

    assert!(has(&lines, "alpha"), "{lines:?}");
}
