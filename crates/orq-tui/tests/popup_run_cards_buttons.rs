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

struct Fake {
    status: &'static str,
    calls: Vec<String>,
}

impl BrowserBackend for Fake {
    fn load(&mut self) -> Result<Vec<BrowserEntry>, LedgerErrorView> {
        Ok(vec![BrowserEntry { name: "alpha".into(), description: None, status: "pending".into(), depends_on: vec![], test_file: None }])
    }
    fn promote(&mut self, _feature: &str) -> PromoteOutcome {
        PromoteOutcome::Saved
    }
    fn load_runs(&mut self) -> Result<Vec<RunRow>, String> {
        Ok(vec![
            RunRow {
                run_id: "run-1".into(),
                feature: "COM-01".into(),
                status: self.status.into(),
                stage: Some("implement".into()),
                implementer_model: Some("opus".into()),
                reviewer_model: Some("kimi".into()),
                has_review_link: false,
            },
            RunRow {
                run_id: "run-2".into(),
                feature: "COM-02".into(),
                status: "completed".into(),
                stage: None,
                implementer_model: None,
                reviewer_model: None,
                has_review_link: false,
            },
        ])
    }
    fn pause_run(&mut self, run_id: &str) -> Result<String, String> {
        self.calls.push(format!("pause {run_id}"));
        Ok(format!("Pause requested for {run_id}"))
    }
    fn stop_run(&mut self, run_id: &str) -> Result<String, String> {
        self.calls.push(format!("stop {run_id}"));
        Ok(format!("Stopped {run_id}"))
    }
    fn resume_check(&mut self, run_id: &str) -> Result<String, String> {
        self.calls.push(format!("check {run_id}"));
        Ok("gate".into())
    }
    fn resume_run(&mut self, run_id: &str) -> Result<String, String> {
        self.calls.push(format!("resume {run_id}"));
        Ok(format!("Resuming {run_id}"))
    }
}

fn key(c: char) -> Event {
    Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
}

fn click(column: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column, row, modifiers: KeyModifiers::NONE })
}

const SIZE: (u16, u16) = (80, 24);

fn show(backend: &mut Fake, events: Vec<Event>) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(TestBackend::new(SIZE.0, SIZE.1)).unwrap();
    let mut all = vec![key('R')];
    all.extend(events);
    let _ = run_browser(&mut terminal, &model, backend, &mut Script(all.into()));
    let buffer = terminal.backend().buffer();
    (0..SIZE.1).map(|y| (0..SIZE.0).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>()).collect()
}

fn find(rows: &[String], needle: &str) -> Option<(u16, u16)> {
    rows.iter().enumerate().find_map(|(y, row)| {
        row.find(needle).map(|byte| (u16::try_from(row[..byte].chars().count()).unwrap(), u16::try_from(y).unwrap()))
    })
}

fn has(rows: &[String], needle: &str) -> bool {
    find(rows, needle).is_some()
}

#[test]
fn each_run_is_a_bordered_card_with_its_status_and_its_stage_and_models_beneath() {
    let rows = show(&mut Fake { status: "running", calls: vec![] }, vec![]);
    let (_, name_row) = find(&rows, "> COM-01").expect("the selected card");
    let name_line = &rows[usize::from(name_row)];
    assert!(name_line.starts_with('│') && name_line.contains("running"), "{rows:#?}");
    assert!(rows[usize::from(name_row) - 1].starts_with('╭'), "{rows:#?}");
    let detail = &rows[usize::from(name_row) + 1];
    assert!(detail.contains("implement") && detail.contains("opus") && detail.contains("kimi"), "{rows:#?}");
    let (_, other) = find(&rows, "COM-02").expect("the second card");
    assert!(other >= name_row + 4, "one card per run: {rows:#?}");
    assert!(has(&rows, "Selected run"), "{rows:#?}");
}

#[test]
fn the_selected_run_offers_the_buttons_its_status_allows() {
    for (status, shown, hidden) in [
        ("running", vec!["[ Pause (p) ]", "[ Stop (s) ]"], vec!["[ Resume (c) ]"]),
        ("pause_requested", vec!["[ Stop (s) ]"], vec!["[ Pause (p) ]", "[ Resume (c) ]"]),
        ("paused", vec!["[ Resume (c) ]"], vec!["[ Pause (p) ]", "[ Stop (s) ]"]),
        ("failed", vec!["[ Resume (c) ]"], vec!["[ Pause (p) ]", "[ Stop (s) ]"]),
        ("completed", vec![], vec!["[ Pause (p) ]", "[ Stop (s) ]", "[ Resume (c) ]"]),
    ] {
        let rows = show(&mut Fake { status, calls: vec![] }, vec![]);
        for label in shown {
            assert!(has(&rows, label), "{status}: {label}: {rows:#?}");
        }
        for label in hidden {
            assert!(!has(&rows, label), "{status}: {label}: {rows:#?}");
        }
    }
}

#[test]
fn clicking_pause_pauses_the_run() {
    let rows = show(&mut Fake { status: "running", calls: vec![] }, vec![]);
    let (x, y) = find(&rows, "[ Pause (p) ]").unwrap();
    let mut backend = Fake { status: "running", calls: vec![] };
    show(&mut backend, vec![click(x + 2, y)]);
    assert_eq!(backend.calls, vec!["pause run-1"]);
}

#[test]
fn stop_asks_for_y_first_by_key_and_by_click() {
    let mut confirmed = Fake { status: "running", calls: vec![] };
    let asked = show(&mut confirmed, vec![key('s')]);
    assert!(has(&asked, "Stop run-1?"), "{asked:#?}");
    assert!(confirmed.calls.is_empty());

    let mut by_key = Fake { status: "running", calls: vec![] };
    show(&mut by_key, vec![key('s'), key('y')]);
    assert_eq!(by_key.calls, vec!["stop run-1"]);

    let mut cancelled = Fake { status: "running", calls: vec![] };
    let rows = show(&mut cancelled, vec![key('s'), key('n')]);
    assert!(cancelled.calls.is_empty() && has(&rows, "Did not stop run-1"), "{rows:#?}");

    let rows = show(&mut Fake { status: "running", calls: vec![] }, vec![]);
    let (x, y) = find(&rows, "[ Stop (s) ]").unwrap();
    let mut by_click = Fake { status: "running", calls: vec![] };
    show(&mut by_click, vec![click(x + 2, y), key('y')]);
    assert_eq!(by_click.calls, vec!["stop run-1"]);
}

#[test]
fn stop_is_refused_for_a_run_that_is_not_running() {
    let mut backend = Fake { status: "paused", calls: vec![] };
    let rows = show(&mut backend, vec![key('s')]);
    assert!(backend.calls.is_empty(), "{:?}", backend.calls);
    assert!(has(&rows, "run-1 is paused; only a running run can be stopped"), "{rows:#?}");
}

#[test]
fn clicking_resume_asks_to_resume_and_y_resumes() {
    let rows = show(&mut Fake { status: "paused", calls: vec![] }, vec![]);
    let (x, y) = find(&rows, "[ Resume (c) ]").unwrap();
    let mut backend = Fake { status: "paused", calls: vec![] };
    show(&mut backend, vec![click(x + 2, y), key('y')]);
    assert_eq!(backend.calls, vec!["check run-1", "resume run-1"]);
}
