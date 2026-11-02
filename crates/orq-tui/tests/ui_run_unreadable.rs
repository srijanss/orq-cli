use std::collections::VecDeque;
use std::io;

use orq_tui::{
    run_browser, BrowserBackend, BrowserEntry, EventSource, LedgerErrorView, PopupModel, PromoteOutcome, RunRow,
};
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;

struct Script(VecDeque<Event>);

impl EventSource for Script {
    fn next_event(&mut self) -> io::Result<Event> {
        self.0.pop_front().ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "script exhausted"))
    }
}

struct Fake {
    runs: Vec<RunRow>,
    unreadable: usize,
}

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
        Ok(self.runs.clone())
    }
    fn unreadable_runs(&mut self) -> usize {
        self.unreadable
    }
}

fn run_row() -> RunRow {
    RunRow {
        run_id: "run-1".into(),
        feature: "COM-01".into(),
        status: "running".into(),
        stage: Some("implement".into()),
        implementer_model: Some("opus".into()),
        reviewer_model: Some("kimi-k3".into()),
        has_review_link: true,
    }
}

fn show(backend: &mut Fake) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let events = vec![Event::Key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::NONE))];
    let _ = run_browser(&mut terminal, &model, backend, &mut Script(events.into()));
    let buffer = terminal.backend().buffer();
    (0..24u16)
        .map(|y| (0..80u16).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
        .collect()
}

#[test]
fn the_runs_view_says_how_many_runs_could_not_be_read_next_to_the_runs_that_could() {
    let lines = show(&mut Fake { runs: vec![run_row()], unreadable: 2 });

    assert!(lines.iter().any(|l| l.contains("COM-01")), "{lines:?}");
    assert!(lines.iter().any(|l| l.contains("2 runs could not be read")), "{lines:?}");
    let one = show(&mut Fake { runs: vec![run_row()], unreadable: 1 });
    assert!(one.iter().any(|l| l.contains("1 run could not be read")), "{one:?}");
}

#[test]
fn the_runs_view_says_nothing_when_every_run_could_be_read() {
    let lines = show(&mut Fake { runs: vec![run_row()], unreadable: 0 });

    assert!(!lines.iter().any(|l| l.contains("could not be read")), "{lines:?}");
}

#[test]
fn the_selected_run_stays_visible_when_the_warning_takes_a_row() {
    let runs: Vec<RunRow> = (0..40).map(|i| RunRow { feature: format!("FEAT-{i:02}"), ..run_row() }).collect();
    let mut backend = Fake { runs, unreadable: 3 };
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let mut events = vec![Event::Key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::NONE))];
    events.extend((0..39).map(|_| Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))));
    let _ = run_browser(&mut terminal, &model, &mut backend, &mut Script(events.into()));
    let buffer = terminal.backend().buffer();
    let lines: Vec<String> = (0..24u16)
        .map(|y| (0..80u16).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
        .collect();

    assert!(lines.iter().any(|l| l.contains("FEAT-39")), "{lines:?}");
    assert!(lines.iter().any(|l| l.contains("3 runs could not be read")), "{lines:?}");
}
