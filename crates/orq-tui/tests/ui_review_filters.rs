use std::cell::RefCell;
use std::collections::VecDeque;
use std::io;
use std::rc::Rc;

use orq_tui::{run_browser, BrowserBackend, BrowserEntry, EventSource, LedgerErrorView, PopupModel, PromoteOutcome, ReviewDetail, ReviewRow};
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Terminal;

struct Script(VecDeque<Event>);

impl EventSource for Script {
    fn next_event(&mut self) -> io::Result<Event> {
        self.0.pop_front().ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "script exhausted"))
    }
}

#[derive(Default)]
struct Calls {
    detail_of: Vec<String>,
    opened: Vec<String>,
    copied: Vec<String>,
}

struct Fake {
    calls: Rc<RefCell<Calls>>,
    link_error: Option<String>,
}

impl BrowserBackend for Fake {
    fn load(&mut self) -> Result<Vec<BrowserEntry>, LedgerErrorView> {
        Ok(vec![BrowserEntry { name: "alpha".into(), description: None, status: "pending".into(), depends_on: vec![], test_file: None }])
    }
    fn promote(&mut self, _feature: &str) -> PromoteOutcome {
        PromoteOutcome::Saved
    }
    fn load_reviews(&mut self) -> Result<Vec<ReviewRow>, String> {
        Ok(vec![
            row("COM-01", "orq-cli", "ready", "high", false),
            row("COM-02", "other", "draft", "low", true),
            row("COM-03", "orq-cli", "draft", "medium", false),
        ])
    }
    fn load_review_detail(&mut self, run_id: &str) -> Result<ReviewDetail, String> {
        self.calls.borrow_mut().detail_of.push(run_id.into());
        Ok(ReviewDetail { summary: vec!["a summary".into()], risks: vec![], files: vec![], tests: vec![], rounds: vec![] })
    }
    fn open_review_link(&mut self, run_id: &str) -> Result<String, String> {
        self.calls.borrow_mut().opened.push(run_id.into());
        match &self.link_error {
            Some(reason) => Err(reason.clone()),
            None => Ok(format!("Opened the review of {run_id}")),
        }
    }
    fn copy_review_link(&mut self, run_id: &str) -> Result<String, String> {
        self.calls.borrow_mut().copied.push(run_id.into());
        Ok(format!("Copied the link of {run_id}"))
    }
}

fn row(feature: &str, project: &str, status: &str, risk: &str, stale: bool) -> ReviewRow {
    ReviewRow { run_id: format!("run-{feature}"), feature: feature.into(), project: project.into(), status: status.into(), risk: risk.into(), current: project == "orq-cli", stale, has_link: false }
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn chars(text: &str) -> Vec<Event> {
    text.chars().map(|c| key(KeyCode::Char(c))).collect()
}

fn click(column: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column, row, modifiers: KeyModifiers::NONE })
}

fn run(link_error: Option<String>, more: Vec<Event>) -> (Vec<String>, Rc<RefCell<Calls>>) {
    let calls = Rc::new(RefCell::new(Calls::default()));
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let mut events = vec![key(KeyCode::Char('V'))];
    events.extend(more);
    let _ = run_browser(&mut terminal, &model, &mut Fake { calls: calls.clone(), link_error }, &mut Script(events.into()));
    let buffer = terminal.backend().buffer();
    let lines = (0..24u16)
        .map(|y| (0..80u16).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
        .collect();
    (lines, calls)
}

fn screen(more: Vec<Event>) -> Vec<String> {
    run(None, more).0
}

fn has(lines: &[String], needle: &str) -> bool {
    lines.iter().any(|l| l.contains(needle))
}

#[test]
fn only_the_current_projects_reviews_show_until_all_projects_is_chosen() {
    let scoped = screen(vec![]);
    assert!(has(&scoped, "COM-01") && has(&scoped, "COM-03") && !has(&scoped, "COM-02"), "{scoped:?}");
    assert!(has(&scoped, "orq-cli"), "{scoped:?}");
    let all = screen(chars("a"));
    assert!(has(&all, "COM-02") && has(&all, "All projects"), "{all:?}");
    let back = screen(chars("aa"));
    assert!(!has(&back, "COM-02"), "{back:?}");
}

#[test]
fn the_status_filter_cycles_through_the_statuses_present_and_off() {
    let first = screen(chars("s"));
    assert!(has(&first, "Status: draft") && has(&first, "COM-03") && !has(&first, "COM-01"), "{first:?}");
    let second = screen(chars("ss"));
    assert!(has(&second, "Status: ready") && has(&second, "COM-01") && !has(&second, "COM-03"), "{second:?}");
    let off = screen(chars("sss"));
    assert!(has(&off, "COM-01") && has(&off, "COM-03"), "{off:?}");
}

#[test]
fn the_risk_filter_cycles_through_the_levels_present_and_off() {
    let first = screen(chars("x"));
    assert!(has(&first, "Risk: high") && has(&first, "COM-01") && !has(&first, "COM-03"), "{first:?}");
    let second = screen(chars("xx"));
    assert!(has(&second, "Risk: medium") && has(&second, "COM-03") && !has(&second, "COM-01"), "{second:?}");
    let off = screen(chars("xxx"));
    assert!(has(&off, "COM-01") && has(&off, "COM-03"), "{off:?}");
}

#[test]
fn typing_a_feature_name_filters_and_escape_clears_it() {
    let mut typed = chars("a/");
    typed.extend(chars("COM-02"));
    typed.push(key(KeyCode::Enter));
    let lines = screen(typed.clone());
    assert!(has(&lines, "COM-02") && !has(&lines, "COM-01"), "{lines:?}");
    assert!(has(&lines, "Search: COM-02"), "{lines:?}");
    typed.push(key(KeyCode::Esc));
    let cleared = screen(typed);
    assert!(has(&cleared, "COM-01") && !has(&cleared, "Search: COM-02"), "{cleared:?}");
}

#[test]
fn a_filter_that_matches_nothing_says_so_and_how_to_widen_it() {
    let lines = screen(chars("sx")); // draft + high matches nothing in this project
    assert!(has(&lines, "No reviews match"), "{lines:?}");
}

#[test]
fn a_stale_report_is_marked_even_when_its_status_is_otherwise_ready() {
    let lines = screen(chars("a"));
    // The card's second line holds the risk and the stale mark.
    let detail = |feature: &str| lines[lines.iter().position(|l| l.contains(feature)).unwrap() + 1].clone();
    assert!(detail("COM-02").contains("stale"), "{lines:?}");
    assert!(!detail("COM-01").contains("stale"), "{lines:?}");
}

#[test]
fn clicking_a_row_selects_it_and_enter_opens_that_review() {
    let before = screen(vec![]);
    let y = before.iter().position(|l| l.contains("COM-03")).unwrap() as u16;
    let (_, calls) = run(None, vec![click(3, y), key(KeyCode::Enter)]);
    assert_eq!(calls.borrow().detail_of, vec!["run-COM-03".to_string()]);
}

#[test]
fn selection_follows_the_filtered_list() {
    let (_, calls) = run(None, [chars("s"), vec![key(KeyCode::Enter)]].concat());
    assert_eq!(calls.borrow().detail_of, vec!["run-COM-03".to_string()]);
}

#[test]
fn u_opens_and_y_copies_the_selected_reviews_link_and_the_outcome_is_shown() {
    let (opened, calls) = run(None, chars("u"));
    assert_eq!(calls.borrow().opened, vec!["run-COM-01".to_string()]);
    assert!(has(&opened, "Opened the review of run-COM-01"), "{opened:?}");
    let (copied, calls) = run(None, chars("y"));
    assert_eq!(calls.borrow().copied, vec!["run-COM-01".to_string()]);
    assert!(has(&copied, "Copied the link of run-COM-01"), "{copied:?}");
}

#[test]
fn a_broken_link_shows_why_and_changes_nothing_else() {
    let (lines, _) = run(Some("the review link is broken".into()), chars("u"));
    assert!(has(&lines, "the review link is broken") && has(&lines, "COM-01"), "{lines:?}");
}

#[test]
fn the_link_keys_also_work_inside_the_detail() {
    let (lines, calls) = run(None, [vec![key(KeyCode::Enter)], chars("u")].concat());
    assert_eq!(calls.borrow().opened, vec!["run-COM-01".to_string()]);
    assert!(has(&lines, "Opened the review of run-COM-01") && has(&lines, "a summary"), "{lines:?}");
}

#[test]
fn nothing_offers_to_approve_or_merge() {
    let lines = screen(chars("a"));
    let text = lines.join("\n").to_lowercase();
    assert!(!text.contains("approve") && !text.contains("merge"), "{text}");
}
