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
        Ok(vec![BrowserEntry { name: "alpha".into(), description: None, status: "pending".into(), depends_on: vec![], test_file: None }])
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
        }])
    }
}

fn show(width: u16, events: Vec<Event>) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
    let _ = run_browser(&mut terminal, &model, &mut Fake, &mut Script(events.into()));
    let buffer = terminal.backend().buffer();
    (0..24u16).map(|y| (0..width).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>()).collect()
}

fn click(column: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column, row, modifiers: KeyModifiers::NONE })
}

#[test]
fn the_runs_tab_is_called_orchestrations_in_the_tab_bar_and_the_title_and_clicking_it_opens_the_runs() {
    let rows = show(80, vec![Event::Key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::NONE))]);
    let bar = &rows[2];
    let at = bar.find("Orchestrations").unwrap_or_else(|| panic!("{rows:#?}"));
    assert!(!bar.contains("Runs"), "{rows:#?}");
    assert!(bar.find("Settings") < Some(at) && Some(at) < bar.find("Reviews"), "{rows:#?}");
    assert!(rows[4].starts_with("Orchestrations"), "{rows:#?}");

    let column = u16::try_from(bar[..at].chars().count()).unwrap() + 2;
    let clicked = show(80, vec![click(column, 2)]);
    assert!(clicked.iter().any(|r| r.contains("COM-01")), "{clicked:#?}");
}

#[test]
fn when_the_longer_tab_and_the_add_button_do_not_both_fit_the_add_button_stays_and_r_still_opens_the_runs() {
    let rows = show(60, vec![]);
    assert!(rows[2].contains("[ + Add Feature ]") && !rows[2].contains("Orchestrations"), "{rows:#?}");
    let opened = show(60, vec![Event::Key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::NONE))]);
    assert!(opened.iter().any(|r| r.contains("COM-01")), "{opened:#?}");
}
