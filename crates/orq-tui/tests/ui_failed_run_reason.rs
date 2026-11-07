use std::collections::VecDeque;
use std::io;

use orq_tui::{run_browser, BrowserBackend, BrowserEntry, EventSource, LedgerErrorView, PopupModel, PromoteOutcome, RunRow};
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
    rows: Vec<RunRow>,
}

fn run(run_id: &str, status: &str, reason: Option<String>) -> RunRow {
    RunRow {
        run_id: run_id.into(),
        feature: format!("feat-{run_id}"),
        status: status.into(),
        stage: None,
        implementer_model: None,
        reviewer_model: None,
        has_review_link: false,
        reason,
    }
}

impl BrowserBackend for Fake {
    fn load(&mut self) -> Result<Vec<BrowserEntry>, LedgerErrorView> {
        Ok(vec![])
    }
    fn promote(&mut self, _feature: &str) -> PromoteOutcome {
        PromoteOutcome::Saved
    }
    fn load_runs(&mut self) -> Result<Vec<RunRow>, String> {
        Ok(self.rows.clone())
    }
}

fn show(rows: Vec<RunRow>) -> String {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(TestBackend::new(80, 30)).unwrap();
    let events = vec![Event::Key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::NONE))];
    let _ = run_browser(&mut terminal, &model, &mut Fake { rows }, &mut Script(events.into()));
    let buffer = terminal.backend().buffer();
    (0..30u16).map(|y| (0..80u16).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>()).collect::<Vec<_>>().join("\n")
}

fn long_reason() -> String {
    let filler = (0..120).map(|n| format!("word{n}")).collect::<Vec<_>>().join(" ");
    format!("the implement session reported a problem: HEADMARK {filler} TAILMARK")
}

#[test]
fn the_selected_failed_run_shows_its_whole_reason_wrapped_over_several_lines() {
    let screen = show(vec![run("run-1", "failed", Some(long_reason()))]);
    for part in ["HEADMARK", "word60", "word119", "TAILMARK"] {
        assert!(screen.contains(part), "{part} is shown: {screen}");
    }
}

#[test]
fn a_blocked_run_shows_its_reason_too() {
    let screen = show(vec![run("run-1", "blocked", Some("waiting on a human decision".into()))]);
    assert!(screen.contains("waiting on a human decision"), "{screen}");
}

#[test]
fn a_run_that_did_not_fail_shows_no_reason() {
    let screen = show(vec![run("run-1", "running", Some("stale reason text".into()))]);
    assert!(!screen.contains("stale reason text"), "{screen}");
}
