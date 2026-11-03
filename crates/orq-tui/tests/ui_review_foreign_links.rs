use std::cell::RefCell;
use std::collections::VecDeque;
use std::io;
use std::rc::Rc;

use orq_tui::{run_browser, BrowserBackend, BrowserEntry, EventSource, LedgerErrorView, PopupModel, PromoteOutcome, ReviewRow};
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;

struct Script(VecDeque<Event>);

impl EventSource for Script {
    fn next_event(&mut self) -> io::Result<Event> {
        self.0.pop_front().ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "script exhausted"))
    }
}

struct Fake(Rc<RefCell<Vec<String>>>);

impl BrowserBackend for Fake {
    fn load(&mut self) -> Result<Vec<BrowserEntry>, LedgerErrorView> {
        Ok(vec![BrowserEntry { name: "alpha".into(), description: None, status: "pending".into(), depends_on: vec![], test_file: None }])
    }
    fn promote(&mut self, _feature: &str) -> PromoteOutcome {
        PromoteOutcome::Saved
    }
    fn load_reviews(&mut self) -> Result<Vec<ReviewRow>, String> {
        Ok(vec![ReviewRow { run_id: "run-other".into(), feature: "APP-07".into(), project: "other".into(), status: "ready".into(), risk: "low".into(), current: false, stale: false }])
    }
    fn open_review_link(&mut self, run_id: &str) -> Result<String, String> {
        self.0.borrow_mut().push(run_id.into());
        Ok("opened".into())
    }
    fn copy_review_link(&mut self, run_id: &str) -> Result<String, String> {
        self.0.borrow_mut().push(run_id.into());
        Ok("copied".into())
    }
}

fn play(keys: &str) -> (Vec<String>, Vec<String>) {
    let calls = Rc::new(RefCell::new(Vec::new()));
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let mut events = vec![Event::Key(KeyEvent::new(KeyCode::Char('V'), KeyModifiers::NONE)), Event::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE))];
    events.extend(keys.chars().map(|c| Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))));
    let _ = run_browser(&mut terminal, &model, &mut Fake(calls.clone()), &mut Script(events.into()));
    let buffer = terminal.backend().buffer();
    let lines = (0..24u16)
        .map(|y| (0..80u16).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
        .collect();
    let recorded = calls.borrow().clone();
    (lines, recorded)
}

#[test]
fn another_projects_review_cannot_open_or_copy_a_link_and_says_why() {
    for key in ["u", "y"] {
        let (lines, calls) = play(key);
        assert!(calls.is_empty(), "{key} reached the backend for a foreign review: {calls:?}");
        assert!(lines.iter().any(|l| l.contains("another project")), "{key}: {lines:?}");
    }
}
