//! A run whose work was merged into the target branch by hand is listed as merged: its card offers
//! only Clear, which asks first and then discards it, and its completed feature reads completed.

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
}

fn run(run_id: &str, feature: &str, status: &str) -> RunRow {
    RunRow {
        run_id: run_id.into(),
        feature: feature.into(),
        status: status.into(),
        stage: None,
        implementer_model: None,
        reviewer_model: None,
        has_review_link: false,
        reason: None,
    }
}

impl BrowserBackend for Fake {
    fn load(&mut self) -> Result<Vec<BrowserEntry>, LedgerErrorView> {
        Ok(vec![BrowserEntry { name: "landed".into(), description: None, status: "completed".into(), depends_on: vec![], test_file: None }])
    }
    fn promote(&mut self, _feature: &str) -> PromoteOutcome {
        PromoteOutcome::Saved
    }
    fn load_runs(&mut self) -> Result<Vec<RunRow>, String> {
        Ok(vec![run("run-1", "landed", "merged")])
    }
    fn feature_runs(&mut self) -> Vec<RunRow> {
        vec![run("run-1", "landed", "merged")]
    }
    fn discard_run(&mut self, run_id: &str) -> Result<String, String> {
        self.calls.push(format!("discard {run_id}"));
        Ok(format!("Discarded run {run_id}."))
    }
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn click((column, row): (u16, u16)) -> Event {
    Event::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column, row, modifiers: KeyModifiers::NONE })
}

fn show(backend: &mut Fake, events: Vec<Event>) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    let _ = run_browser(&mut terminal, &model, backend, &mut Script(events.into()));
    let buffer = terminal.backend().buffer();
    (0..30u16).map(|y| (0..100u16).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>()).collect()
}

fn line_of<'a>(screen: &'a [String], feature: &str) -> (u16, &'a String) {
    let (y, line) = screen
        .iter()
        .enumerate()
        .find(|(_, line)| line.contains(&format!(" {feature} ")))
        .unwrap_or_else(|| panic!("{feature} is listed: {screen:#?}"));
    (y as u16, line)
}

const TO_RUNS: KeyCode = KeyCode::Char('R');

#[test]
fn a_merged_run_offers_only_clear_which_asks_first_and_y_discards_it() {
    let screen = show(&mut Fake::default(), vec![key(TO_RUNS)]);
    let (y, line) = line_of(&screen, "landed");
    assert!(line.contains("[Clear]"), "{screen:#?}");
    for other in ["[Accept]", "[Discard]", "[Resume]", "[Review again]"] {
        assert!(!line.contains(other), "{other}: {line:?}");
    }
    let at = (line[..line.find("[Clear]").unwrap()].chars().count() as u16 + 1, y);

    let mut backend = Fake::default();
    let asked = show(&mut backend, vec![key(TO_RUNS), click(at)]);
    assert!(asked.iter().any(|line| line.contains("Clear run-1?") && line.contains("stay")), "{asked:#?}");
    assert!(backend.calls.is_empty(), "{:?}", backend.calls);

    let mut backend = Fake::default();
    let cancelled = show(&mut backend, vec![key(TO_RUNS), click(at), key(KeyCode::Char('n'))]);
    assert!(backend.calls.is_empty(), "{:?}", backend.calls);
    assert!(cancelled.iter().any(|line| line.contains("Did not clear run-1")), "{cancelled:#?}");

    let mut backend = Fake::default();
    let done = show(&mut backend, vec![key(TO_RUNS), click(at), key(KeyCode::Char('y'))]);
    assert_eq!(backend.calls, vec!["discard run-1"]);
    assert!(done.iter().any(|line| line.contains("Discarded run run-1.")), "{done:#?}");
}

#[test]
fn a_completed_feature_whose_latest_run_was_merged_by_hand_reads_completed() {
    let screen = show(&mut Fake::default(), vec![]);
    let (_, line) = line_of(&screen, "landed");
    assert!(line.contains("completed") && !line.contains("in_progress"), "{line:?}");
}
