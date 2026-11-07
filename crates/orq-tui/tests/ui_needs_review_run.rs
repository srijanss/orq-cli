//! A run whose review escalated waits for a human: its card offers to review again, accept or
//! discard, and selecting it shows why it stopped.

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

struct Fake;

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
            feature: "waiting".into(),
            status: "needs_review".into(),
            stage: None,
            implementer_model: None,
            reviewer_model: None,
            has_review_link: false,
            reason: Some("the review escalated after 3 rounds with 1 finding open".into()),
        }])
    }
}

fn show(events: Vec<Event>) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(TestBackend::new(110, 30)).unwrap();
    let _ = run_browser(&mut terminal, &model, &mut Fake, &mut Script(events.into()));
    let buffer = terminal.backend().buffer();
    (0..30u16).map(|y| (0..110u16).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>()).collect()
}

#[test]
fn a_run_waiting_for_a_review_decision_offers_review_again_accept_and_discard_and_says_why() {
    let screen = show(vec![Event::Key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::NONE))]);

    let card = screen.iter().find(|l| l.contains(" waiting ")).unwrap_or_else(|| panic!("{screen:#?}"));
    assert!(card.contains("[Review again] [Accept] [Discard]"), "{screen:#?}");
    assert!(screen.iter().any(|l| l.contains("the review escalated after 3 rounds with 1 finding open")), "{screen:#?}");
}
