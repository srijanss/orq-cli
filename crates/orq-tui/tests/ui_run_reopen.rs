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
    reopened: Vec<String>,
    reopen: PaneFocus,
    focus: PaneFocus,
}

impl BrowserBackend for Fake {
    fn load(&mut self) -> Result<Vec<BrowserEntry>, LedgerErrorView> {
        Ok(vec![])
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
            has_review_link: true,
        }])
    }
    fn focus_run_pane(&mut self, _run_id: &str) -> PaneFocus {
        self.focus.clone()
    }
    fn reopen_run_pane(&mut self, run_id: &str) -> PaneFocus {
        self.reopened.push(run_id.into());
        self.reopen.clone()
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
fn a_missing_pane_offers_a_fresh_one_and_o_opens_it() {
    let mut backend = Fake {
        reopened: vec![],
        reopen: PaneFocus::Focused,
        focus: PaneFocus::Missing("the pane of run-1 no longer exists".into()),
    };

    let lines = show(&mut backend, vec![key(KeyCode::Char('R')), key(KeyCode::Enter)]);
    assert!(has(&lines, "no longer exists"), "{lines:?}");
    assert!(has(&lines, "press o"), "the recovery is offered: {lines:?}");
    assert!(backend.reopened.is_empty());

    let lines = show(&mut backend, vec![key(KeyCode::Char('R')), key(KeyCode::Enter), key(KeyCode::Char('o'))]);
    assert_eq!(backend.reopened, vec!["run-1".to_string()]);
    assert!(has(&lines, "Opened a new pane for run-1"), "{lines:?}");
}

#[test]
fn a_failed_reopen_says_why() {
    let mut backend = Fake {
        reopened: vec![],
        reopen: PaneFocus::Failed("server_not_running: no herdr server".into()),
        focus: PaneFocus::Focused,
    };

    let lines = show(&mut backend, vec![key(KeyCode::Char('R')), key(KeyCode::Char('o'))]);

    assert!(has(&lines, "no herdr server"), "{lines:?}");
}
