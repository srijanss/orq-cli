use std::collections::VecDeque;
use std::io;

use orq_tui::{run_browser, BrowserBackend, BrowserEntry, EventSource, LedgerErrorView, PopupModel, PromoteOutcome, ReviewRow};
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Terminal;

struct Script(VecDeque<Event>);

impl EventSource for Script {
    fn next_event(&mut self) -> io::Result<Event> {
        self.0.pop_front().ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "script exhausted"))
    }
}

struct Fake(Result<Vec<ReviewRow>, String>);

impl BrowserBackend for Fake {
    fn load(&mut self) -> Result<Vec<BrowserEntry>, LedgerErrorView> {
        Ok(vec![BrowserEntry { name: "alpha".into(), description: None, status: "pending".into(), depends_on: vec![], test_file: None }])
    }
    fn promote(&mut self, _feature: &str) -> PromoteOutcome {
        PromoteOutcome::Saved
    }
    fn load_reviews(&mut self) -> Result<Vec<ReviewRow>, String> {
        self.0.clone()
    }
}

fn row(feature: &str, project: &str, status: &str, risk: &str) -> ReviewRow {
    ReviewRow { run_id: format!("run-{feature}"), feature: feature.into(), project: project.into(), status: status.into(), risk: risk.into(), current: true, stale: false }
}

fn click(column: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column, row, modifiers: KeyModifiers::NONE })
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn show(reviews: Result<Vec<ReviewRow>, String>, events: Vec<Event>) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let _ = run_browser(&mut terminal, &model, &mut Fake(reviews), &mut Script(events.into()));
    let buffer = terminal.backend().buffer();
    (0..24u16)
        .map(|y| (0..80u16).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
        .collect()
}

fn has(lines: &[String], needle: &str) -> bool {
    lines.iter().any(|l| l.contains(needle))
}

fn inbox() -> Result<Vec<ReviewRow>, String> {
    Ok(vec![row("COM-01", "orq-cli", "ready", "high"), row("COM-02", "orq-cli", "draft", "low")])
}

#[test]
fn the_tab_bar_lists_reviews_after_runs() {
    let lines = show(inbox(), vec![]);
    let bar = &lines[2];
    assert!(bar.find("Runs") < bar.find("Reviews") && bar.contains("Reviews"), "{lines:?}");
}

#[test]
fn clicking_the_reviews_tab_lists_each_review_with_project_status_and_risk() {
    let lines = show(inbox(), vec![click(36, 2)]);
    assert!(has(&lines, "COM-01") && has(&lines, "orq-cli") && has(&lines, "ready") && has(&lines, "high"), "{lines:?}");
    assert!(!has(&lines, "alpha"), "{lines:?}");
}

#[test]
fn the_keyboard_opens_the_tab_moves_and_leaves_it() {
    let opened = show(inbox(), vec![key(KeyCode::Char('V'))]);
    assert!(has(&opened, "COM-01"), "{opened:?}");
    let moved = show(inbox(), vec![key(KeyCode::Char('V')), key(KeyCode::Down)]);
    assert!(moved.iter().any(|l| l.contains("COM-02")), "{moved:?}");
    let left = show(inbox(), vec![key(KeyCode::Char('V')), key(KeyCode::Esc)]);
    assert!(has(&left, "alpha"), "{left:?}");
}

#[test]
fn clicking_the_features_tab_leaves_the_reviews_view() {
    let lines = show(inbox(), vec![click(36, 2), click(2, 2)]);
    assert!(has(&lines, "alpha") && !has(&lines, "COM-01"), "{lines:?}");
}

#[test]
fn an_empty_inbox_says_so() {
    let lines = show(Ok(vec![]), vec![click(36, 2)]);
    assert!(has(&lines, "No reviews yet"), "{lines:?}");
}

#[test]
fn an_unreadable_inbox_shows_why() {
    let lines = show(Err("the review store is locked".into()), vec![click(36, 2)]);
    assert!(has(&lines, "the review store is locked"), "{lines:?}");
}

#[test]
fn there_is_no_approve_or_merge_action() {
    let lines = show(inbox(), vec![click(36, 2)]);
    let text = lines.join("\n").to_lowercase();
    assert!(!text.contains("approve") && !text.contains("merge"), "{text}");
}

#[test]
fn hostile_text_in_a_row_cannot_drive_the_terminal() {
    let lines = show(Ok(vec![row("x\u{1b}[2J", "orq-cli", "ready", "low")]), vec![click(36, 2)]);
    assert!(!lines.iter().any(|l| l.contains('\u{1b}')), "{lines:?}");
}

#[test]
fn in_a_narrow_popup_the_tab_hides_but_the_keyboard_still_reaches_it() {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(TestBackend::new(60, 24)).unwrap();
    let _ = run_browser(&mut terminal, &model, &mut Fake(inbox()), &mut Script(vec![key(KeyCode::Char('V'))].into()));
    let buffer = terminal.backend().buffer();
    let text: String = (0..24u16).flat_map(|y| (0..60u16).map(move |x| (x, y))).map(|(x, y)| buffer[(x, y)].symbol().to_string()).collect();
    assert!(text.contains("COM-01"), "{text}");
}
