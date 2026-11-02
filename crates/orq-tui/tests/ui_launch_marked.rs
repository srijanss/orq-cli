use std::collections::VecDeque;
use std::io;

use orq_tui::{
    run_browser, BrowserBackend, BrowserEntry, EventSource, LaunchJob, LedgerErrorView, PopupModel, PromoteOutcome,
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

struct Done(Option<Result<String, String>>);

impl LaunchJob for Done {
    fn poll(&mut self) -> Option<Result<String, String>> {
        self.0.take()
    }
}

struct Fake {
    started: Vec<(String, String, String)>,
}

impl BrowserBackend for Fake {
    fn load(&mut self) -> Result<Vec<BrowserEntry>, LedgerErrorView> {
        let entry = |name: &str, status: &str| BrowserEntry {
            name: name.into(),
            description: None,
            status: status.into(),
            depends_on: vec![],
            test_file: None,
        };
        Ok(vec![entry("alpha", "pending"), entry("beta", "pending"), entry("gamma", "pending")])
    }
    fn promote(&mut self, _feature: &str) -> PromoteOutcome {
        PromoteOutcome::Saved
    }
    fn launch_profiles(&mut self) -> Result<Vec<String>, String> {
        Ok(vec!["opus".into(), "sonnet".into()])
    }
    fn start_launch(&mut self, feature: &str, implementer: &str, reviewer: &str) -> Box<dyn LaunchJob> {
        self.started.push((feature.into(), implementer.into(), reviewer.into()));
        Box::new(Done(Some(Ok(format!("run-{feature}")))))
    }
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn show(backend: &mut Fake, events: Vec<Event>) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let _ = run_browser(&mut terminal, &model, backend, &mut Script(events.into()));
    let buffer = terminal.backend().buffer();
    (0..24u16)
        .map(|y| (0..80u16).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
        .collect()
}

fn has(lines: &[String], needle: &str) -> bool {
    lines.iter().any(|l| l.contains(needle))
}

#[test]
fn l_with_marked_features_picks_once_and_starts_one_run_per_marked_feature_in_list_order() {
    let mut backend = Fake { started: vec![] };
    // Mark gamma, then alpha (so marking order differs from list order), then launch.
    let events = vec![
        key(KeyCode::Down),
        key(KeyCode::Down),
        key(KeyCode::Down),
        key(KeyCode::Char(' ')),
        key(KeyCode::Up),
        key(KeyCode::Up),
        key(KeyCode::Char(' ')),
        key(KeyCode::Char('L')),
        key(KeyCode::Enter),
        key(KeyCode::Down),
        key(KeyCode::Enter),
    ];

    let lines = show(&mut backend, events);

    let started: Vec<_> = backend.started.iter().map(|(f, i, r)| (f.as_str(), i.as_str(), r.as_str())).collect();
    assert_eq!(started, vec![("alpha", "opus", "sonnet"), ("gamma", "opus", "sonnet")]);
    assert!(has(&lines, "Started"), "{lines:?}");
}

#[test]
fn the_picker_names_how_many_features_it_launches() {
    let mut backend = Fake { started: vec![] };
    let events = vec![
        key(KeyCode::Down),
        key(KeyCode::Char(' ')),
        key(KeyCode::Down),
        key(KeyCode::Char(' ')),
        key(KeyCode::Char('L')),
    ];

    let lines = show(&mut backend, events);

    assert!(has(&lines, "Launch 2 features"), "{lines:?}");
    assert!(backend.started.is_empty());
}

#[test]
fn l_without_marks_still_launches_the_selected_feature() {
    let mut backend = Fake { started: vec![] };
    let events = vec![
        key(KeyCode::Down),
        key(KeyCode::Down),
        key(KeyCode::Char('L')),
        key(KeyCode::Enter),
        key(KeyCode::Enter),
    ];

    show(&mut backend, events);

    assert_eq!(backend.started.len(), 1);
    assert_eq!(backend.started[0].0, "beta");
}
