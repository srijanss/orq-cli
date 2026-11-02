use std::collections::VecDeque;
use std::io;

use orq_tui::{
    run_browser, BrowserBackend, BrowserEntry, EventSource, LedgerErrorView, PopupModel, PromoteOutcome, RunRow,
};
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;

struct Script(VecDeque<Event>);

impl EventSource for Script {
    fn next_event(&mut self) -> io::Result<Event> {
        self.0.pop_front().ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "script exhausted"))
    }
}

struct Fake {
    runs: Result<Vec<RunRow>, String>,
    loads: usize,
}

impl BrowserBackend for Fake {
    fn load(&mut self) -> Result<Vec<BrowserEntry>, LedgerErrorView> {
        Ok(vec![BrowserEntry {
            name: "alpha".into(),
            description: None,
            status: "pending".into(),
            depends_on: vec![],
            test_file: None,
        }])
    }
    fn promote(&mut self, _feature: &str) -> PromoteOutcome {
        PromoteOutcome::Saved
    }
    fn load_runs(&mut self) -> Result<Vec<RunRow>, String> {
        self.loads += 1;
        self.runs.clone()
    }
}

fn run_row(id: &str, feature: &str, status: &str, stage: Option<&str>) -> RunRow {
    RunRow {
        run_id: id.into(),
        feature: feature.into(),
        status: status.into(),
        stage: stage.map(str::to_string),
        implementer_model: Some("opus".into()),
        reviewer_model: Some("kimi-k3".into()),
    }
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn show(backend: &mut Fake, events: Vec<Event>) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let (width, height) = (80u16, 24u16);
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    let _ = run_browser(&mut terminal, &model, backend, &mut Script(events.into()));
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| (0..width).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
        .collect()
}

fn has(lines: &[String], needle: &str) -> bool {
    lines.iter().any(|l| l.contains(needle))
}

#[test]
fn r_lists_the_projects_runs_with_feature_status_stage_and_models() {
    let mut backend = Fake {
        runs: Ok(vec![
            run_row("run-1", "COM-01", "running", Some("implement")),
            run_row("run-2", "COM-02", "failed", Some("gate")),
        ]),
        loads: 0,
    };

    let lines = show(&mut backend, vec![key(KeyCode::Char('R'))]);

    let first = lines.iter().find(|l| l.contains("COM-01")).expect("COM-01 row");
    assert!(first.contains("running") && first.contains("implement"), "{first}");
    assert!(first.contains("opus") && first.contains("kimi-k3"), "{first}");
    let second = lines.iter().find(|l| l.contains("COM-02")).expect("COM-02 row");
    assert!(second.contains("failed") && second.contains("gate"), "{second}");
    assert!(!has(&lines, "alpha"), "{lines:?}");
}

fn click(column: u16, row: u16) -> Event {
    use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

fn two_runs() -> Fake {
    Fake {
        runs: Ok(vec![
            run_row("run-1", "COM-01", "running", Some("implement")),
            run_row("run-2", "COM-02", "failed", Some("gate")),
        ]),
        loads: 0,
    }
}

fn selected_line(backend: &mut Fake, events: Vec<Event>) -> String {
    use ratatui::style::Modifier;
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let _ = run_browser(&mut terminal, &model, backend, &mut Script(events.into()));
    let buffer = terminal.backend().buffer();
    (0..24u16)
        .find(|&y| buffer[(0, y)].modifier.contains(Modifier::REVERSED) && buffer[(0, y)].symbol() != " ")
        .map(|y| (0..80u16).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>())
        .unwrap_or_default()
}

#[test]
fn the_arrow_keys_move_the_selected_run_and_stop_at_the_ends() {
    let r = key(KeyCode::Char('R'));

    assert!(selected_line(&mut two_runs(), vec![r.clone()]).contains("COM-01"));
    let down = selected_line(&mut two_runs(), vec![r.clone(), key(KeyCode::Down), key(KeyCode::Down)]);
    assert!(down.contains("COM-02"), "{down}");
    let up = selected_line(&mut two_runs(), vec![r, key(KeyCode::Down), key(KeyCode::Up), key(KeyCode::Up)]);
    assert!(up.contains("COM-01"), "{up}");
}

#[test]
fn clicking_a_run_selects_it() {
    let line = selected_line(&mut two_runs(), vec![key(KeyCode::Char('R')), click(3, 6)]);

    assert!(line.contains("COM-02"), "{line}");
}

#[test]
fn lowercase_r_reloads_the_runs_and_esc_returns_to_the_features() {
    let mut backend = two_runs();
    let lines = show(&mut backend, vec![key(KeyCode::Char('R')), key(KeyCode::Char('r'))]);
    assert_eq!(backend.loads, 2);
    assert!(has(&lines, "COM-01"), "{lines:?}");

    let lines = show(&mut two_runs(), vec![key(KeyCode::Char('R')), key(KeyCode::Esc)]);
    assert!(has(&lines, "alpha") && !has(&lines, "COM-01"), "{lines:?}");
}

#[test]
fn no_runs_and_an_unreadable_store_each_say_so() {
    let mut none = Fake { runs: Ok(vec![]), loads: 0 };
    assert!(has(&show(&mut none, vec![key(KeyCode::Char('R'))]), "No runs yet"));

    let mut broken = Fake { runs: Err("run data is corrupt".into()), loads: 0 };
    assert!(has(&show(&mut broken, vec![key(KeyCode::Char('R'))]), "run data is corrupt"));
}

fn many_runs(count: usize) -> Fake {
    Fake {
        runs: Ok((1..=count).map(|n| run_row(&format!("run-{n}"), &format!("COM-{n:02}"), "running", Some("implement"))).collect()),
        loads: 0,
    }
}

#[test]
fn the_list_scrolls_to_keep_the_selected_run_visible_and_leaves_the_hint_row_alone() {
    let mut keys = vec![key(KeyCode::Char('R'))];
    keys.extend((0..25).map(|_| key(KeyCode::Down)));

    let lines = show(&mut many_runs(30), keys.clone());
    let selected = selected_line(&mut many_runs(30), keys);

    assert!(selected.contains("COM-26"), "{selected}");
    assert!(has(&lines, "COM-26") && !has(&lines, "COM-01"), "{lines:?}");
    assert!(has(&lines, "close") || has(&lines, "move"), "the hint row is still shown: {lines:?}");
}

#[test]
fn the_last_body_row_is_the_hint_so_the_run_beneath_it_is_reached_by_scrolling() {
    let lines = show(&mut many_runs(30), vec![key(KeyCode::Char('R'))]);

    let last_run = lines.iter().rposition(|l| l.contains("COM-")).expect("a run row");
    let hint = lines.iter().position(|l| l.contains("move")).expect("hint row");
    assert!(last_run < hint, "{lines:?}");
}

#[test]
fn clicking_after_scrolling_selects_the_run_shown_on_that_row() {
    let mut keys = vec![key(KeyCode::Char('R'))];
    keys.extend((0..25).map(|_| key(KeyCode::Down)));
    keys.push(click(3, 5));

    let line = selected_line(&mut many_runs(30), keys);

    assert!(line.contains("COM-"), "{line}");
    assert!(!line.contains("COM-26") && !line.contains("COM-01"), "{line}");
}
