use std::cell::RefCell;
use std::collections::VecDeque;
use std::io;
use std::rc::Rc;

use orq_tui::{run_browser, BrowserBackend, BrowserEntry, EventSource, LedgerErrorView, PopupModel, PromoteOutcome, ReviewDetail, ReviewRow};
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
struct Calls {
    opened: Vec<String>,
    copied: Vec<String>,
}

struct Fake {
    calls: Rc<RefCell<Calls>>,
    open_fails: bool,
}

impl BrowserBackend for Fake {
    fn load(&mut self) -> Result<Vec<BrowserEntry>, LedgerErrorView> {
        Ok(vec![BrowserEntry { name: "alpha".into(), description: None, status: "pending".into(), depends_on: vec![], test_file: None }])
    }
    fn promote(&mut self, _feature: &str) -> PromoteOutcome {
        PromoteOutcome::Saved
    }
    fn load_reviews(&mut self) -> Result<Vec<ReviewRow>, String> {
        Ok(vec![ReviewRow { run_id: "run-1".into(), feature: "APP-01".into(), project: "orq-cli".into(), status: "ready".into(), risk: "low".into(), current: true, stale: false }])
    }
    fn load_review_detail(&mut self, _run_id: &str) -> Result<ReviewDetail, String> {
        self.load_review_detail_with_links(_run_id).map(|(d, _)| d)
    }
    fn load_review_detail_with_links(&mut self, _run_id: &str) -> Result<(ReviewDetail, Vec<Option<String>>), String> {
        let detail = ReviewDetail {
            summary: vec![],
            risks: vec!["high a.rs:1 first".into(), "low b.rs:2 second".into()],
            files: vec![],
            tests: vec![],
            rounds: vec![],
        };
        Ok((detail, vec![Some("https://example.invalid/a.rs#L1".into()), None]))
    }
    fn open_url(&mut self, url: &str) -> Result<String, String> {
        self.calls.borrow_mut().opened.push(url.into());
        if self.open_fails { Err("no browser".into()) } else { Ok("Opened the link".into()) }
    }
    fn copy_url(&mut self, url: &str) -> Result<String, String> {
        self.calls.borrow_mut().copied.push(url.into());
        Ok("Copied the link".into())
    }
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

/// Opens the first review's Risks section, then plays `keys`.
fn play(open_fails: bool, keys: Vec<KeyCode>) -> (Vec<String>, Calls) {
    let calls = Rc::new(RefCell::new(Calls::default()));
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let mut events = vec![key(KeyCode::Char('V')), key(KeyCode::Enter), key(KeyCode::Char('2'))];
    events.extend(keys.into_iter().map(key));
    let _ = run_browser(&mut terminal, &model, &mut Fake { calls: calls.clone(), open_fails }, &mut Script(events.into()));
    let buffer = terminal.backend().buffer();
    let lines = (0..24u16)
        .map(|y| (0..80u16).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
        .collect();
    let recorded = std::mem::take(&mut *calls.borrow_mut());
    (lines, recorded)
}

fn has(lines: &[String], needle: &str) -> bool {
    lines.iter().any(|l| l.contains(needle))
}

#[test]
fn enter_on_a_risk_with_a_verified_link_opens_it() {
    let (lines, calls) = play(false, vec![KeyCode::Enter]);
    assert_eq!(calls.opened, ["https://example.invalid/a.rs#L1"]);
    assert!(has(&lines, "Opened the link"), "{lines:?}");
}

#[test]
fn a_failed_open_falls_back_to_copying_the_url() {
    let (lines, calls) = play(true, vec![KeyCode::Enter]);
    assert_eq!(calls.copied, ["https://example.invalid/a.rs#L1"]);
    assert!(has(&lines, "no browser") && has(&lines, "Copied"), "{lines:?}");
}

#[test]
fn a_risk_without_a_verified_link_offers_no_link_action() {
    let (lines, calls) = play(false, vec![KeyCode::Down, KeyCode::Enter]);
    assert!(calls.opened.is_empty() && calls.copied.is_empty(), "{:?} {:?}", calls.opened, calls.copied);
    assert!(!has(&lines, "Enter open"), "{lines:?}");
}

#[test]
fn the_hint_offers_the_link_action_only_while_a_linked_risk_is_selected() {
    let (lines, _) = play(false, vec![]);
    assert!(has(&lines, "Enter open"), "{lines:?}");
}
