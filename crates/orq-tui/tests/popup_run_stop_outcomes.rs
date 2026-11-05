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

/// Three runs; a successful stop leaves the second one failed, as the store would.
struct Fake {
    stop: Result<String, String>,
    stopped: bool,
    calls: Vec<String>,
}

fn row(run_id: &str, feature: &str, status: &str) -> RunRow {
    RunRow {
        run_id: run_id.into(),
        feature: feature.into(),
        status: status.into(),
        stage: Some("implement".into()),
        implementer_model: None,
        reviewer_model: None,
        has_review_link: false,
    }
}

impl BrowserBackend for Fake {
    fn load(&mut self) -> Result<Vec<BrowserEntry>, LedgerErrorView> {
        Ok(vec![BrowserEntry { name: "alpha".into(), description: None, status: "pending".into(), depends_on: vec![], test_file: None }])
    }
    fn promote(&mut self, _feature: &str) -> PromoteOutcome {
        PromoteOutcome::Saved
    }
    fn load_runs(&mut self) -> Result<Vec<RunRow>, String> {
        self.calls.push("load".into());
        let second = if self.stopped { "failed" } else { "running" };
        Ok(vec![row("run-1", "COM-01", "running"), row("run-2", "COM-02", second), row("run-3", "COM-03", "running")])
    }
    fn stop_run(&mut self, run_id: &str) -> Result<String, String> {
        self.calls.push(format!("stop {run_id}"));
        self.stopped = self.stop.is_ok();
        self.stop.clone()
    }
    fn resume_check(&mut self, run_id: &str) -> Result<String, String> {
        self.calls.push(format!("check {run_id}"));
        Ok("where it stopped".into())
    }
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn chars(text: &str) -> Vec<Event> {
    text.chars().map(|c| key(KeyCode::Char(c))).collect()
}

fn show(backend: &mut Fake, width: u16, height: u16, events: Vec<Event>) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    let _ = run_browser(&mut terminal, &model, backend, &mut Script(events.into()));
    let buffer = terminal.backend().buffer();
    (0..height).map(|y| (0..width).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>()).collect()
}

fn has(rows: &[String], needle: &str) -> bool {
    rows.iter().any(|row| row.contains(needle))
}

#[test]
fn a_successful_stop_reloads_the_runs_keeps_the_selection_and_the_message_and_c_then_resumes() {
    let mut backend = Fake { stop: Ok("Stopped run-2; c resumes it".into()), stopped: false, calls: vec![] };
    let mut events = vec![key(KeyCode::Char('R')), key(KeyCode::Down)];
    events.extend(chars("sy"));
    let rows = show(&mut backend, 80, 24, events);
    assert_eq!(backend.calls, vec!["load", "stop run-2", "load"]);
    assert!(has(&rows, "Stopped run-2; c resumes it"), "{rows:#?}");
    let selected = rows.iter().position(|r| r.contains("> COM-02")).expect("run-2 stays selected");
    assert!(rows[selected].contains("failed"), "{rows:#?}");
    assert!(has(&rows, "[ Resume (c) ]"), "{rows:#?}");

    let mut backend = Fake { stop: Ok("Stopped run-2".into()), stopped: false, calls: vec![] };
    let mut events = vec![key(KeyCode::Char('R')), key(KeyCode::Down)];
    events.extend(chars("syc"));
    let rows = show(&mut backend, 80, 24, events);
    assert_eq!(backend.calls.last().map(String::as_str), Some("check run-2"), "{:?}", backend.calls);
    assert!(has(&rows, "Resume run-2"), "{rows:#?}");
}

#[test]
fn a_failed_stop_shows_why_and_a_second_y_does_not_try_again() {
    let mut backend = Fake { stop: Err("cannot verify the owner of run-1".into()), stopped: false, calls: vec![] };
    let mut events = vec![key(KeyCode::Char('R'))];
    events.extend(chars("syy"));
    let rows = show(&mut backend, 80, 24, events);
    assert_eq!(backend.calls.iter().filter(|c| c.starts_with("stop")).count(), 1, "{:?}", backend.calls);
    assert!(!backend.calls.contains(&"load".to_string()) || backend.calls.iter().filter(|c| *c == "load").count() == 1);
    let selected = rows.iter().position(|r| r.contains("> COM-01")).expect("run-1 selected");
    assert!(rows[selected].contains("running"), "{rows:#?}");

    let mut backend = Fake { stop: Err("cannot verify the owner of run-1".into()), stopped: false, calls: vec![] };
    let mut events = vec![key(KeyCode::Char('R'))];
    events.extend(chars("sy"));
    let rows = show(&mut backend, 80, 24, events);
    assert!(has(&rows, "cannot verify the owner of run-1"), "{rows:#?}");
    assert!(!has(&rows, "Stopped"), "{rows:#?}");
}

#[test]
fn a_body_too_short_for_a_card_lists_each_run_on_one_line_with_its_status() {
    for height in [2u16, 3] {
        let mut backend = Fake { stop: Ok(String::new()), stopped: false, calls: vec![] };
        let rows = show(&mut backend, 80, height, vec![key(KeyCode::Char('R'))]);
        let selected = rows.iter().find(|r| r.contains("> COM-01")).unwrap_or_else(|| panic!("{height}: {rows:#?}"));
        assert!(selected.contains("running"), "{height}: {rows:#?}");
    }
    let mut backend = Fake { stop: Ok(String::new()), stopped: false, calls: vec![] };
    let rows = show(&mut backend, 80, 3, vec![key(KeyCode::Char('R')), key(KeyCode::Down)]);
    assert!(has(&rows, "COM-01") && rows.iter().any(|r| r.contains("> COM-02")), "{rows:#?}");
}

fn click(column: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column, row, modifiers: KeyModifiers::NONE })
}

#[test]
fn clicking_a_one_line_row_selects_that_run_and_s_then_stops_it() {
    let mut backend = Fake { stop: Ok("Stopped run-2".into()), stopped: false, calls: vec![] };
    let mut events = vec![key(KeyCode::Char('R')), click(3, 1)];
    events.extend(chars("sy"));
    show(&mut backend, 80, 3, events);
    assert!(backend.calls.contains(&"stop run-2".to_string()), "{:?}", backend.calls);

    // One row visible and scrolled to the second run: the row on screen is run-2, not run-1.
    let mut backend = Fake { stop: Ok("Stopped run-2".into()), stopped: false, calls: vec![] };
    let mut events = vec![key(KeyCode::Char('R')), key(KeyCode::Down), click(3, 0)];
    events.extend(chars("sy"));
    show(&mut backend, 80, 2, events);
    assert!(backend.calls.contains(&"stop run-2".to_string()), "{:?}", backend.calls);
}

#[test]
fn clicking_an_unselected_row_of_a_scrolled_list_selects_the_run_shown_there() {
    // Two rows fit; selecting the third run scrolls the list so rows show run-2 then run-3.
    let mut backend = Fake { stop: Ok("Stopped run-2".into()), stopped: false, calls: vec![] };
    let mut events = vec![key(KeyCode::Char('R')), key(KeyCode::Down), key(KeyCode::Down), click(3, 0)];
    events.extend(chars("sy"));
    show(&mut backend, 80, 3, events);
    let stops: Vec<&String> = backend.calls.iter().filter(|c| c.starts_with("stop")).collect();
    assert_eq!(stops, vec!["stop run-2"], "{:?}", backend.calls);
}
