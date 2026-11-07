use std::collections::VecDeque;
use std::io;

use orq_tui::{run_browser, BrowserBackend, BrowserEntry, EventSource, LedgerErrorView, PopupModel, PromoteOutcome, RunRow};
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
struct Fake {
    calls: Vec<String>,
    open_fails: Option<&'static str>,
    copy_fails: Option<&'static str>,
}

fn run(run_id: &str, feature: &str, status: &str, has_review_link: bool) -> RunRow {
    RunRow {
        run_id: run_id.into(),
        feature: feature.into(),
        status: status.into(),
        stage: None,
        implementer_model: None,
        reviewer_model: None,
        has_review_link,
        reason: None,
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
        Ok(vec![
            run("run-1", "linked", "completed", true),
            run("run-2", "busy", "running", true),
            run("run-3", "unlinked", "running", false),
        ])
    }
    fn open_review_link(&mut self, run_id: &str) -> Result<String, String> {
        self.calls.push(format!("open {run_id}"));
        if let Some(why) = self.open_fails {
            return Err(why.to_string());
        }
        Ok(format!("Opened the link of {run_id}"))
    }
    fn copy_review_link(&mut self, run_id: &str) -> Result<String, String> {
        self.calls.push(format!("copy {run_id}"));
        if let Some(why) = self.copy_fails {
            return Err(why.to_string());
        }
        Ok(format!("Copied the link of {run_id}"))
    }
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn click((column, row): (u16, u16)) -> Event {
    Event::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column, row, modifiers: KeyModifiers::NONE })
}

fn show(backend: &mut Fake, width: u16, events: Vec<Event>) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(TestBackend::new(width, 30)).unwrap();
    let _ = run_browser(&mut terminal, &model, backend, &mut Script(events.into()));
    let buffer = terminal.backend().buffer();
    (0..30u16).map(|y| (0..width).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>()).collect()
}

fn line_of<'a>(screen: &'a [String], feature: &str) -> (u16, &'a String) {
    let (y, line) = screen
        .iter()
        .enumerate()
        .find(|(_, line)| line.contains(&format!(" {feature} ")))
        .unwrap_or_else(|| panic!("{feature} is listed: {screen:#?}"));
    (y as u16, line)
}

fn button(screen: &[String], feature: &str, label: &str) -> (u16, u16) {
    let (y, line) = line_of(screen, feature);
    let byte = line.find(label).unwrap_or_else(|| panic!("{feature} has {label}: {line:?}"));
    (line[..byte].chars().count() as u16 + 1, y)
}

const TO_RUNS: KeyCode = KeyCode::Char('R');

#[test]
fn a_run_with_a_review_link_has_buttons_that_open_and_copy_that_runs_link() {
    let screen = show(&mut Fake::default(), 100, vec![key(TO_RUNS)]);

    let mut backend = Fake::default();
    let opened = show(&mut backend, 100, vec![key(TO_RUNS), click(button(&screen, "busy", "[Open MR]"))]);
    assert_eq!(backend.calls, vec!["open run-2"]);
    assert!(opened.iter().any(|line| line.contains("Opened the link of run-2")), "{opened:#?}");

    let mut backend = Fake::default();
    let copied = show(&mut backend, 100, vec![key(TO_RUNS), click(button(&screen, "linked", "[Copy link]"))]);
    assert_eq!(backend.calls, vec!["copy run-1"]);
    assert!(copied.iter().any(|line| line.contains("Copied the link of run-1")), "{copied:#?}");
}

#[test]
fn a_run_without_a_review_link_has_no_link_buttons() {
    let screen = show(&mut Fake::default(), 100, vec![key(TO_RUNS)]);
    let (_, line) = line_of(&screen, "unlinked");
    assert!(!line.contains("[Open MR]") && !line.contains("[Copy link]"), "{line:?}");
    assert!(line.contains("[Pause]") && line.contains("[Stop]"), "{line:?}");
}

#[test]
fn a_card_too_narrow_for_the_link_buttons_keeps_its_status_buttons_and_they_still_act() {
    // Wide enough for [Pause] [Stop] beside the marker, too narrow once the link buttons join them.
    let width = 36;
    let screen = show(&mut Fake::default(), width, vec![key(TO_RUNS)]);
    let (_, line) = line_of(&screen, "busy");
    assert!(line.contains("[Pause]") && line.contains("[Stop]"), "{line:?}");
    assert!(!line.contains("[Open MR]") && !line.contains("[Copy link]"), "{line:?}");
}

#[test]
fn a_status_button_still_acts_when_the_link_buttons_do_not_fit() {
    let width = 36;
    let screen = show(&mut Fake::default(), width, vec![key(TO_RUNS)]);
    let mut backend = Fake::default();
    let after = show(&mut backend, width, vec![key(TO_RUNS), click(button(&screen, "busy", "[Stop]"))]);
    assert!(after.iter().any(|line| line.contains("Stop run-2?")), "{after:#?}");
    assert!(backend.calls.is_empty(), "{:?}", backend.calls);
}

#[test]
fn a_link_that_cannot_be_opened_is_copied_instead_and_the_notice_says_both() {
    let width = 200;
    let screen = show(&mut Fake::default(), width, vec![key(TO_RUNS)]);
    let mut backend = Fake { open_fails: Some("no opener worked"), ..Fake::default() };
    let after = show(&mut backend, width, vec![key(TO_RUNS), click(button(&screen, "busy", "[Open MR]"))]);
    assert_eq!(backend.calls, vec!["open run-2", "copy run-2"]);
    assert!(after.iter().any(|line| line.contains("no opener worked; Copied the link of run-2")), "{after:#?}");
}

#[test]
fn when_neither_open_nor_copy_works_the_notice_gives_both_reasons() {
    let width = 200;
    let screen = show(&mut Fake::default(), width, vec![key(TO_RUNS)]);
    let mut backend = Fake { open_fails: Some("no opener worked"), copy_fails: Some("no clipboard; the link is L"), ..Fake::default() };
    let after = show(&mut backend, width, vec![key(TO_RUNS), click(button(&screen, "busy", "[Open MR]"))]);
    assert!(after.iter().any(|line| line.contains("no opener worked; no clipboard; the link is L")), "{after:#?}");
}

#[test]
fn a_reason_that_is_about_the_run_not_the_desktop_is_said_once() {
    let width = 200;
    let screen = show(&mut Fake::default(), width, vec![key(TO_RUNS)]);
    let mut backend = Fake { open_fails: Some("run-2 has no review request yet"), copy_fails: Some("run-2 has no review request yet"), ..Fake::default() };
    let after = show(&mut backend, width, vec![key(TO_RUNS), click(button(&screen, "busy", "[Open MR]"))]);
    assert!(after.iter().any(|line| line.contains("run-2 has no review request yet")), "{after:#?}");
    assert!(!after.iter().any(|line| line.contains("yet; run-2")), "{after:#?}");
}
