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

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    ManyLinked,
    Mismatched,
    BothFail,
}

struct Fake {
    opened: Rc<RefCell<Vec<String>>>,
    mode: Mode,
}

const COUNT: usize = 40;

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
    fn load_review_detail(&mut self, run_id: &str) -> Result<ReviewDetail, String> {
        self.load_review_detail_with_links(run_id).map(|(d, _)| d)
    }
    fn load_review_detail_with_links(&mut self, _run_id: &str) -> Result<(ReviewDetail, Vec<Option<String>>), String> {
        let risks: Vec<String> = (0..COUNT).map(|i| format!("high f{i}.rs:1 risk number {i}")).collect();
        let mut links: Vec<Option<String>> = (0..COUNT).map(|i| Some(format!("https://example.invalid/{i}"))).collect();
        if self.mode == Mode::Mismatched {
            links.truncate(COUNT - 1);
        }
        Ok((ReviewDetail { summary: vec![], risks, files: vec![], tests: vec![], rounds: vec![] }, links))
    }
    fn open_url(&mut self, url: &str) -> Result<String, String> {
        self.opened.borrow_mut().push(url.into());
        if self.mode == Mode::BothFail { Err("open failed".into()) } else { Ok("Opened".into()) }
    }
    fn copy_url(&mut self, _url: &str) -> Result<String, String> {
        Err("copy failed".into())
    }
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn play(mode: Mode, keys: Vec<KeyCode>) -> (Vec<String>, Vec<String>) {
    let opened = Rc::new(RefCell::new(Vec::new()));
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let mut events = vec![key(KeyCode::Char('V')), key(KeyCode::Enter), key(KeyCode::Char('2'))];
    events.extend(keys.into_iter().map(key));
    let _ = run_browser(&mut terminal, &model, &mut Fake { opened: opened.clone(), mode }, &mut Script(events.into()));
    let buffer = terminal.backend().buffer();
    let lines = (0..24u16)
        .map(|y| (0..80u16).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
        .collect();
    let recorded = opened.borrow().clone();
    (lines, recorded)
}

#[test]
fn paging_moves_the_selected_risk_so_enter_opens_the_risk_that_is_shown() {
    let (lines, opened) = play(Mode::ManyLinked, vec![KeyCode::PageDown, KeyCode::Enter]);
    let selected = lines.iter().find(|l| l.contains('›')).unwrap_or_else(|| panic!("no selected row visible: {lines:?}"));
    let n: usize = selected.rsplit("number ").next().unwrap().trim().parse().unwrap();
    assert!(n > 0, "paging did not move the selection: {selected}");
    assert_eq!(opened, [format!("https://example.invalid/{n}")]);
}

#[test]
fn paging_back_up_returns_to_the_first_risk() {
    let (lines, opened) = play(Mode::ManyLinked, vec![KeyCode::PageDown, KeyCode::PageUp, KeyCode::Enter]);
    assert!(lines.iter().any(|l| l.contains('›') && l.contains("number 0")), "{lines:?}");
    assert_eq!(opened, ["https://example.invalid/0"]);
}

#[test]
fn links_that_do_not_match_the_risks_are_dropped_and_nothing_is_opened() {
    let (lines, opened) = play(Mode::Mismatched, vec![KeyCode::Enter]);
    assert!(opened.is_empty(), "{opened:?}");
    assert!(!lines.iter().any(|l| l.contains("Enter open")), "{lines:?}");
}

#[test]
fn when_opening_and_copying_both_fail_both_reasons_are_shown() {
    let (lines, opened) = play(Mode::BothFail, vec![KeyCode::Enter]);
    assert_eq!(opened.len(), 1);
    assert!(lines.iter().any(|l| l.contains("open failed") && l.contains("copy failed")), "{lines:?}");
}
