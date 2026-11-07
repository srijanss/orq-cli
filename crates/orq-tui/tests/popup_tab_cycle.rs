use std::collections::VecDeque;
use std::io;

use orq_tui::{run_browser, BrowserBackend, BrowserEntry, EventSource, LedgerErrorView, PopupModel, PromoteOutcome, ReviewRow, RunRow};
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;

struct Script(VecDeque<Event>);

impl EventSource for Script {
    fn next_event(&mut self) -> io::Result<Event> {
        self.0.pop_front().ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "script exhausted"))
    }
}

#[derive(Default)]
struct Fake {
    calls: Vec<&'static str>,
}

impl BrowserBackend for Fake {
    fn load(&mut self) -> Result<Vec<BrowserEntry>, LedgerErrorView> {
        Ok(vec![BrowserEntry { name: "alpha".into(), description: None, status: "pending".into(), depends_on: vec![], test_file: None }])
    }
    fn promote(&mut self, _feature: &str) -> PromoteOutcome {
        PromoteOutcome::Saved
    }
    fn load_runs(&mut self) -> Result<Vec<RunRow>, String> {
        self.calls.push("runs");
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
    fn load_reviews(&mut self) -> Result<Vec<ReviewRow>, String> {
        self.calls.push("reviews");
        Ok(vec![ReviewRow {
            run_id: "run-9".into(),
            feature: "REV-09".into(),
            project: "orq-cli".into(),
            status: "done".into(),
            risk: "-".into(),
            current: true,
            stale: false,
            has_link: false,
        }])
    }
}

fn tabs(n: usize) -> Vec<Event> {
    (0..n).map(|_| Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))).collect()
}

fn show(backend: &mut Fake, events: Vec<Event>) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    let _ = run_browser(&mut terminal, &model, backend, &mut Script(events.into()));
    let buffer = terminal.backend().buffer();
    (0..24u16).map(|y| (0..100u16).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>()).collect()
}

fn has(rows: &[String], needle: &str) -> bool {
    rows.iter().any(|row| row.contains(needle))
}

#[test]
fn tab_moves_from_features_to_settings_to_orchestrations_to_reviews_and_back_to_features() {
    let mut backend = Fake::default();
    let settings = show(&mut backend, tabs(1));
    assert!(has(&settings, "Implementer") && !has(&settings, "COM-01"), "{settings:#?}");

    let mut backend = Fake::default();
    let runs = show(&mut backend, tabs(2));
    assert!(has(&runs, "COM-01") && !has(&runs, "Implementer"), "{runs:#?}");
    assert_eq!(backend.calls, vec!["runs"], "the runs are loaded when Tab opens them");

    let mut backend = Fake::default();
    let reviews = show(&mut backend, tabs(3));
    assert!(has(&reviews, "REV-09") && !has(&reviews, "COM-01"), "{reviews:#?}");
    assert_eq!(backend.calls, vec!["runs", "reviews"], "the reviews are loaded when Tab opens them");

    let mut backend = Fake::default();
    let features = show(&mut backend, tabs(4));
    assert!(has(&features, "alpha") && !has(&features, "REV-09"), "{features:#?}");

    let mut backend = Fake::default();
    let again = show(&mut backend, tabs(5));
    assert!(has(&again, "Implementer"), "{again:#?}");
}

#[test]
fn esc_on_settings_still_returns_to_features_and_the_settings_hint_names_the_next_tab() {
    let mut backend = Fake::default();
    let settings = show(&mut backend, tabs(1));
    assert!(has(&settings, "Tab orchestrations"), "{settings:#?}");

    let mut backend = Fake::default();
    let back = show(&mut backend, vec![tabs(1).remove(0), Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))]);
    assert!(has(&back, "alpha") && backend.calls.is_empty(), "{back:#?}");
}
