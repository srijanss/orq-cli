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

struct Fake {
    launched: Vec<(String, String, String)>,
    result: Result<String, String>,
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
        Ok(vec![entry("alpha", "pending"), entry("beta", "draft")])
    }
    fn promote(&mut self, _feature: &str) -> PromoteOutcome {
        PromoteOutcome::Saved
    }
    fn launch_profiles(&mut self) -> Result<Vec<String>, String> {
        Ok(vec!["opus".into(), "sonnet".into(), "kimi".into()])
    }
    fn launch(&mut self, feature: &str, implementer: &str, reviewer: &str) -> Result<String, String> {
        self.launched.push((feature.into(), implementer.into(), reviewer.into()));
        self.result.clone()
    }
}

fn fake() -> Fake {
    Fake { launched: vec![], result: Ok("run-7".into()) }
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
fn l_on_a_pending_feature_picks_an_implementer_then_a_reviewer_and_launches() {
    let mut backend = fake();

    let lines = show(
        &mut backend,
        vec![key(KeyCode::Down), key(KeyCode::Char('L')), key(KeyCode::Enter), key(KeyCode::Down), key(KeyCode::Enter)],
    );

    assert_eq!(backend.launched, vec![("alpha".to_string(), "opus".to_string(), "sonnet".to_string())]);
    assert!(has(&lines, "run-7"), "{lines:?}");
}

#[test]
fn the_picker_lists_the_profiles_and_esc_launches_nothing() {
    let mut backend = fake();
    let open = show(&mut backend, vec![key(KeyCode::Down), key(KeyCode::Char('L'))]);
    assert!(has(&open, "opus") && has(&open, "kimi") && has(&open, "alpha"), "{open:?}");

    let lines = show(&mut backend, vec![key(KeyCode::Down), key(KeyCode::Char('L')), key(KeyCode::Enter), key(KeyCode::Esc)]);

    assert!(backend.launched.is_empty());
    assert!(!has(&lines, "opus"), "{lines:?}");
}

#[test]
fn only_a_pending_feature_can_be_launched_and_a_failed_launch_says_why() {
    let mut backend = fake();
    let lines = show(&mut backend, vec![key(KeyCode::Down), key(KeyCode::Down), key(KeyCode::Char('L'))]);
    assert!(has(&lines, "Only pending features can be launched"), "{lines:?}");
    assert!(!has(&lines, "opus"), "{lines:?}");

    let mut failing = Fake { launched: vec![], result: Err("busy: another run holds the project".into()) };
    let lines = show(
        &mut failing,
        vec![key(KeyCode::Down), key(KeyCode::Char('L')), key(KeyCode::Enter), key(KeyCode::Enter)],
    );
    assert!(has(&lines, "busy: another run holds the project"), "{lines:?}");
}
