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

type Calls = Rc<RefCell<Vec<String>>>;

struct Fake(Vec<ReviewRow>, Calls);

impl BrowserBackend for Fake {
    fn load(&mut self) -> Result<Vec<BrowserEntry>, LedgerErrorView> {
        Ok(vec![])
    }
    fn promote(&mut self, _feature: &str) -> PromoteOutcome {
        PromoteOutcome::Saved
    }
    fn load_reviews(&mut self) -> Result<Vec<ReviewRow>, String> {
        Ok(self.0.clone())
    }
    fn open_review_link(&mut self, run_id: &str) -> Result<String, String> {
        self.1.borrow_mut().push(format!("open {run_id}"));
        Ok("opened".into())
    }
    fn copy_review_link(&mut self, run_id: &str) -> Result<String, String> {
        self.1.borrow_mut().push(format!("copy {run_id}"));
        Ok("copied".into())
    }
    fn load_review_detail(&mut self, run_id: &str) -> Result<ReviewDetail, String> {
        self.1.borrow_mut().push(format!("details {run_id}"));
        Err("no report in this test".into())
    }
}

fn review(n: usize, current: bool) -> ReviewRow {
    ReviewRow {
        run_id: format!("run-{n}"),
        feature: format!("COM-{n:02}"),
        project: if current { "orq-cli".into() } else { "other".into() },
        status: "ready".into(),
        risk: "low".into(),
        current,
        stale: false,
        has_link: current,
    }
}

fn click(column: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column, row, modifiers: KeyModifiers::NONE })
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn screen_after(rows: &[ReviewRow], events: Vec<Event>) -> (Vec<String>, Vec<String>) {
    let calls: Calls = Rc::default();
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let mut script = vec![key(KeyCode::Char('V'))];
    script.extend(events);
    let _ = run_browser(&mut terminal, &model, &mut Fake(rows.to_vec(), calls.clone()), &mut Script(script.into()));
    let buffer = terminal.backend().buffer();
    let lines = (0..24u16).map(|y| (0..80u16).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string()).collect();
    let recorded = calls.borrow().clone();
    (lines, recorded)
}

/// The screen position of the first `label` on the row showing `feature`, in the screen after `events`.
fn button(rows: &[ReviewRow], events: &[Event], feature: &str, label: &str) -> (u16, u16) {
    let (lines, _) = screen_after(rows, events.to_vec());
    let y = lines.iter().position(|l| l.contains(feature)).unwrap_or_else(|| panic!("{feature} is shown: {lines:?}"));
    let x = lines[y].chars().take_while({
        let mut rest = lines[y].as_str();
        move |_| {
            let found = rest.starts_with(label);
            let mut chars = rest.chars();
            chars.next();
            rest = chars.as_str();
            !found
        }
    });
    (u16::try_from(x.count()).unwrap(), u16::try_from(y).unwrap())
}

fn linked(count: usize) -> Vec<ReviewRow> {
    (1..=count).map(|n| review(n, true)).collect()
}

#[test]
fn open_mr_opens_the_link_of_its_own_card_not_the_selected_one() {
    let rows = linked(3);
    let (x, y) = button(&rows, &[], "COM-02", "[Open MR]");
    let (_, calls) = screen_after(&rows, vec![click(x + 2, y)]);
    assert_eq!(calls, ["open run-2"]);
}

#[test]
fn copy_link_copies_the_link_of_its_own_card() {
    let rows = linked(3);
    let (x, y) = button(&rows, &[], "COM-03", "[Copy link]");
    let (_, calls) = screen_after(&rows, vec![click(x + 2, y)]);
    assert_eq!(calls, ["copy run-3"]);
}

#[test]
fn details_opens_the_review_of_its_own_card() {
    let rows = linked(3);
    let (x, y) = button(&rows, &[], "COM-02", "[Details]");
    let (lines, calls) = screen_after(&rows, vec![click(x + 2, y)]);
    assert_eq!(calls, ["details run-2"], "{lines:?}");
    assert!(lines.iter().any(|l| l.contains("no report in this test")), "{lines:?}");
}

#[test]
fn a_button_press_also_selects_its_card() {
    let rows = linked(3);
    let (x, y) = button(&rows, &[], "COM-03", "[Open MR]");
    let (lines, _) = screen_after(&rows, vec![click(x + 2, y)]);
    assert!(lines.iter().any(|l| l.contains("> COM-03")), "{lines:?}");
}

#[test]
fn buttons_work_after_the_list_has_scrolled() {
    let rows = linked(8);
    let scroll = vec![key(KeyCode::Down); 7];
    let (x, y) = button(&rows, &scroll, "COM-06", "[Copy link]");
    let (_, calls) = screen_after(&rows, [scroll, vec![click(x + 2, y)]].concat());
    assert_eq!(calls, ["copy run-6"]);
}

#[test]
fn buttons_work_under_a_filter_that_hides_other_reviews() {
    // The review of another project is listed first but hidden by the default project filter.
    let rows = vec![review(1, false), review(2, true), review(3, true)];
    let (x, y) = button(&rows, &[], "COM-03", "[Open MR]");
    let (_, calls) = screen_after(&rows, vec![click(x + 2, y)]);
    assert_eq!(calls, ["open run-3"]);
}

#[test]
fn a_press_on_the_card_outside_its_buttons_only_selects_it() {
    let rows = linked(3);
    let (_, y) = button(&rows, &[], "COM-02", "[Details]");
    let (lines, calls) = screen_after(&rows, vec![click(6, y)]);
    assert!(calls.is_empty(), "{calls:?}");
    assert!(lines.iter().any(|l| l.contains("> COM-02")), "{lines:?}");
}

#[test]
fn a_card_without_a_link_has_no_open_or_copy_to_press() {
    let rows = vec![review(1, false), review(2, true)];
    let (lines, _) = screen_after(&rows, vec![key(KeyCode::Char('a'))]);
    let other = lines.iter().find(|l| l.contains("COM-01")).unwrap();
    assert!(!other.contains("[Open MR]") && other.contains("[Details]"), "{lines:?}");
}
