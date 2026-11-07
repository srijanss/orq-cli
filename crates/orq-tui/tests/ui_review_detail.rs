use std::collections::VecDeque;
use std::io;

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

struct Fake {
    detail: Result<ReviewDetail, String>,
}

impl BrowserBackend for Fake {
    fn load(&mut self) -> Result<Vec<BrowserEntry>, LedgerErrorView> {
        Ok(vec![BrowserEntry { name: "alpha".into(), description: None, status: "pending".into(), depends_on: vec![], test_file: None }])
    }
    fn promote(&mut self, _feature: &str) -> PromoteOutcome {
        PromoteOutcome::Saved
    }
    fn load_reviews(&mut self) -> Result<Vec<ReviewRow>, String> {
        Ok(vec![ReviewRow { run_id: "run-1".into(), feature: "COM-01".into(), project: "orq-cli".into(), status: "ready".into(), risk: "high".into(), current: true, stale: false, has_link: false }])
    }
    fn load_review_detail(&mut self, run_id: &str) -> Result<ReviewDetail, String> {
        assert_eq!(run_id, "run-1");
        self.detail.clone()
    }
}

fn full() -> ReviewDetail {
    ReviewDetail {
        summary: vec!["Adds the reviews tab".into(), "3 files changed".into()],
        risks: vec!["high auth/login.rs:4 token logged".into()],
        files: vec!["src/auth/login.rs  +4 -0".into()],
        tests: vec!["12 passed, 0 failed".into()],
        rounds: vec!["round 1: 2 findings, 1 fixed".into(), "round 2: no summary recorded".into()],
    }
}

fn empty() -> ReviewDetail {
    ReviewDetail { summary: vec![], risks: vec![], files: vec![], tests: vec![], rounds: vec![] }
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn click(column: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column, row, modifiers: KeyModifiers::NONE })
}

fn screen(detail: Result<ReviewDetail, String>, events: Vec<Event>) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let _ = run_browser(&mut terminal, &model, &mut Fake { detail }, &mut Script(events.into()));
    let buffer = terminal.backend().buffer();
    (0..24u16)
        .map(|y| (0..80u16).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
        .collect()
}

fn has(lines: &[String], needle: &str) -> bool {
    lines.iter().any(|l| l.contains(needle))
}

/// Open the Reviews tab (V) and the first review (Enter), then play `more`.
fn opened(detail: Result<ReviewDetail, String>, more: Vec<Event>) -> Vec<String> {
    let mut events = vec![key(KeyCode::Char('V')), key(KeyCode::Enter)];
    events.extend(more);
    screen(detail, events)
}

#[test]
fn enter_opens_the_detail_on_the_summary_with_all_section_names() {
    let lines = opened(Ok(full()), vec![]);
    for name in ["Summary", "Risks", "Files", "Tests", "Review Rounds"] {
        assert!(has(&lines, name), "{name} missing: {lines:?}");
    }
    assert!(has(&lines, "Adds the reviews tab") && !has(&lines, "token logged"), "{lines:?}");
}

#[test]
fn the_keyboard_switches_sections() {
    let risks = opened(Ok(full()), vec![key(KeyCode::Right)]);
    assert!(has(&risks, "token logged") && !has(&risks, "Adds the reviews tab"), "{risks:?}");
    let rounds = opened(Ok(full()), vec![key(KeyCode::Char('5'))]);
    assert!(has(&rounds, "round 2: no summary recorded"), "{rounds:?}");
    let back = opened(Ok(full()), vec![key(KeyCode::Char('5')), key(KeyCode::Left)]);
    assert!(has(&back, "12 passed, 0 failed"), "{back:?}");
}

#[test]
fn clicking_a_section_name_switches_to_it() {
    let before = opened(Ok(full()), vec![]);
    let row = before.iter().position(|l| l.contains("Files")).unwrap();
    let column = before[row].chars().take_while(|_| true).collect::<Vec<_>>();
    let text: String = column.iter().collect();
    let x = text[..text.find("Files").unwrap()].chars().count() as u16;
    let lines = opened(Ok(full()), vec![click(x + 1, row as u16)]);
    assert!(has(&lines, "src/auth/login.rs  +4 -0") && !has(&lines, "Adds the reviews tab"), "{lines:?}");
}

#[test]
fn a_section_with_nothing_recorded_says_so() {
    let lines = opened(Ok(empty()), vec![key(KeyCode::Right)]);
    assert!(has(&lines, "Nothing recorded"), "{lines:?}");
}

#[test]
fn an_unreadable_report_shows_why_instead_of_sections() {
    let lines = opened(Err("report.json is damaged".into()), vec![]);
    assert!(has(&lines, "report.json is damaged"), "{lines:?}");
}

#[test]
fn escape_returns_to_the_list_and_again_to_features() {
    let list = opened(Ok(full()), vec![key(KeyCode::Esc)]);
    assert!(has(&list, "COM-01") && !has(&list, "Adds the reviews tab"), "{list:?}");
    let features = opened(Ok(full()), vec![key(KeyCode::Esc), key(KeyCode::Esc)]);
    assert!(has(&features, "alpha"), "{features:?}");
}

#[test]
fn reopening_a_review_starts_on_the_summary_again() {
    let lines = opened(Ok(full()), vec![key(KeyCode::Right), key(KeyCode::Esc), key(KeyCode::Enter)]);
    assert!(has(&lines, "Adds the reviews tab"), "{lines:?}");
}

#[test]
fn hostile_text_in_a_section_cannot_drive_the_terminal() {
    let mut detail = full();
    detail.summary = vec!["x\u{1b}[2Jy".into()];
    let lines = opened(Ok(detail), vec![]);
    assert!(!lines.iter().any(|l| l.contains('\u{1b}')), "{lines:?}");
}

#[test]
fn a_long_section_scrolls_with_the_arrows_without_leaving_the_detail() {
    let mut detail = full();
    detail.files = (0..60).map(|i| format!("file-{i:02}.rs")).collect();
    let top = opened(Ok(detail.clone()), vec![key(KeyCode::Char('3'))]);
    assert!(has(&top, "file-00.rs") && !has(&top, "file-59.rs"), "{top:?}");
    let scrolled = opened(Ok(detail), vec![key(KeyCode::Char('3')), key(KeyCode::PageDown), key(KeyCode::PageDown), key(KeyCode::PageDown), key(KeyCode::PageDown)]);
    assert!(has(&scrolled, "file-59.rs") && !has(&scrolled, "file-00.rs"), "{scrolled:?}");
}
