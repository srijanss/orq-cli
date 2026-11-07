use std::collections::VecDeque;
use std::io;

use orq_tui::{
    run_browser, BrowserBackend, BrowserEntry, EventSource, LedgerErrorView, PaneFocus, PopupModel, PromoteOutcome,
    RunRow,
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
    focused: Vec<String>,
    outcome: PaneFocus,
}

impl BrowserBackend for Fake {
    fn load(&mut self) -> Result<Vec<BrowserEntry>, LedgerErrorView> {
        Ok(vec![])
    }
    fn promote(&mut self, _feature: &str) -> PromoteOutcome {
        PromoteOutcome::Saved
    }
    fn load_runs(&mut self) -> Result<Vec<RunRow>, String> {
        let run = |id: &str, feature: &str| RunRow {
            run_id: id.into(),
            feature: feature.into(),
            status: "running".into(),
            stage: Some("implement".into()),
            implementer_model: None,
            reviewer_model: None,
            has_review_link: true,
            reason: None,
        };
        Ok(vec![run("run-1", "COM-01"), run("run-2", "COM-02")])
    }
    fn focus_run_pane(&mut self, run_id: &str) -> PaneFocus {
        self.focused.push(run_id.into());
        self.outcome.clone()
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
fn enter_on_the_selected_run_focuses_its_pane_and_says_so() {
    let mut backend = Fake { focused: vec![], outcome: PaneFocus::Focused };

    let lines = show(&mut backend, vec![key(KeyCode::Char('R')), key(KeyCode::Down), key(KeyCode::Enter)]);

    assert_eq!(backend.focused, vec!["run-2".to_string()]);
    assert!(has(&lines, "Focused the pane of run-2") || has(&lines, "run-2"), "{lines:?}");
    assert!(has(&lines, "COM-01"), "the list stays: {lines:?}");
}

#[test]
fn a_missing_or_failed_pane_gives_its_reason_and_the_next_key_clears_it() {
    let mut missing = Fake { focused: vec![], outcome: PaneFocus::Missing("the pane no longer exists".into()) };
    let lines = show(&mut missing, vec![key(KeyCode::Char('R')), key(KeyCode::Enter)]);
    assert!(has(&lines, "the pane no longer exists"), "{lines:?}");

    let mut failed = Fake { focused: vec![], outcome: PaneFocus::Failed("herdr is not running".into()) };
    let lines = show(&mut failed, vec![key(KeyCode::Char('R')), key(KeyCode::Enter)]);
    assert!(has(&lines, "herdr is not running"), "{lines:?}");

    let lines = show(&mut failed, vec![key(KeyCode::Char('R')), key(KeyCode::Enter), key(KeyCode::Down)]);
    assert!(!has(&lines, "herdr is not running"), "{lines:?}");
}

#[test]
fn enter_with_no_runs_asks_for_nothing() {
    struct Empty(Fake);
    impl BrowserBackend for Empty {
        fn load(&mut self) -> Result<Vec<BrowserEntry>, LedgerErrorView> {
            Ok(vec![])
        }
        fn promote(&mut self, _feature: &str) -> PromoteOutcome {
            PromoteOutcome::Saved
        }
        fn load_runs(&mut self) -> Result<Vec<RunRow>, String> {
            Ok(vec![])
        }
        fn focus_run_pane(&mut self, run_id: &str) -> PaneFocus {
            self.0.focused.push(run_id.into());
            PaneFocus::Focused
        }
    }
    let mut backend = Empty(Fake { focused: vec![], outcome: PaneFocus::Focused });
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let events: VecDeque<Event> = vec![key(KeyCode::Char('R')), key(KeyCode::Enter)].into();

    let _ = run_browser(&mut terminal, &model, &mut backend, &mut Script(events));

    assert!(backend.0.focused.is_empty());
}
