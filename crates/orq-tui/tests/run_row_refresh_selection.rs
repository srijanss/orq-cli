use std::collections::VecDeque;
use std::io;

use orq_tui::{run_browser, BrowserBackend, BrowserEntry, EventSource, LedgerErrorView, PopupModel, PromoteOutcome, RunRow};
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Terminal;

const WIDTH: u16 = 100;
const HEIGHT: u16 = 40;

struct Script(VecDeque<Event>);

impl EventSource for Script {
    fn next_event(&mut self) -> io::Result<Event> {
        self.0.pop_front().ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "script exhausted"))
    }
}

#[derive(Default)]
struct Fake {
    calls: Vec<String>,
    /// The runs as the fake's store holds them, oldest first.
    store: Vec<RunRow>,
    /// After an action the store lists its runs in the opposite order.
    reorder: bool,
    /// After an action the runs can no longer be read.
    unreadable_after: bool,
    acted: bool,
}

impl Fake {
    fn with(store: Vec<RunRow>) -> Self {
        Fake { store, ..Fake::default() }
    }
    fn did(&mut self, call: String) {
        self.calls.push(call);
        self.acted = true;
        if self.reorder {
            self.store.reverse();
        }
    }
}

fn entry(name: &str, status: &str) -> BrowserEntry {
    BrowserEntry { name: name.into(), description: None, status: status.into(), depends_on: vec![], test_file: None }
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
    }
}

fn three() -> Vec<RunRow> {
    vec![run("run-a", "alpha", "running"), run("run-b", "bravo", "failed"), run("run-c", "charlie", "blocked")]
}

impl BrowserBackend for Fake {
    fn load(&mut self) -> Result<Vec<BrowserEntry>, LedgerErrorView> {
        Ok(vec![entry("alpha", "in_progress"), entry("bravo", "in_progress"), entry("charlie", "in_progress")])
    }
    fn promote(&mut self, _feature: &str) -> PromoteOutcome {
        PromoteOutcome::Saved
    }
    fn feature_runs(&mut self) -> Vec<RunRow> {
        self.calls.push("feature_runs".into());
        self.store.clone()
    }
    fn load_runs(&mut self) -> Result<Vec<RunRow>, String> {
        self.calls.push("load_runs".into());
        if self.unreadable_after && self.acted {
            return Err("the runs could not be read".into());
        }
        Ok(self.store.clone())
    }
    fn load_reviews(&mut self) -> Result<Vec<orq_tui::ReviewRow>, String> {
        Ok(vec![])
    }
    fn pause_run(&mut self, run_id: &str) -> Result<String, String> {
        if let Some(run) = self.store.iter_mut().find(|run| run.run_id == run_id) {
            run.status = "pause_requested".into();
        }
        self.did(format!("pause {run_id}"));
        Ok(format!("Pause requested for {run_id}"))
    }
    fn stop_run(&mut self, run_id: &str) -> Result<String, String> {
        self.did(format!("stop {run_id}"));
        Ok(format!("Stopped {run_id}"))
    }
    fn resume_check(&mut self, run_id: &str) -> Result<String, String> {
        self.calls.push(format!("check {run_id}"));
        Ok("where it stopped".into())
    }
    fn resume_run(&mut self, run_id: &str) -> Result<String, String> {
        self.did(format!("resume {run_id}"));
        Ok(format!("Resumed {run_id}"))
    }
    fn discard_run(&mut self, run_id: &str) -> Result<String, String> {
        self.store.retain(|run| run.run_id != run_id);
        self.did(format!("discard {run_id}"));
        Ok(format!("Discarded {run_id}"))
    }
}

fn show(backend: &mut Fake, events: Vec<Event>) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).unwrap();
    let _ = run_browser(&mut terminal, &model, backend, &mut Script(events.into()));
    let buffer = terminal.backend().buffer();
    (0..HEIGHT).map(|y| (0..WIDTH).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>()).collect()
}

fn click((column, row): (u16, u16)) -> Event {
    Event::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column, row, modifiers: KeyModifiers::NONE })
}

fn key(c: char) -> Event {
    Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
}

fn press(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn to_runs() -> Vec<Event> {
    vec![press(KeyCode::Tab), press(KeyCode::Tab)]
}

/// Where `button` is drawn on the card of `feature`'s run.
fn run_button(screen: &[String], feature: &str, button: &str) -> (u16, u16) {
    let (y, line) = screen
        .iter()
        .enumerate()
        .find(|(_, line)| line.contains(&format!(" {feature} ")) && line.contains(button))
        .unwrap_or_else(|| panic!("{feature} has {button}: {screen:#?}"));
    let byte = line.find(button).unwrap();
    (line[..byte].chars().count() as u16 + 1, y as u16)
}

/// The feature of the run marked as selected.
fn selected(screen: &[String]) -> Option<String> {
    screen.iter().find_map(|line| {
        let rest = line.split("│> ").nth(1)?;
        Some(rest.split_whitespace().next()?.to_string())
    })
}

/// What the fake was asked to do, leaving out the reads.
fn acted(backend: &Fake) -> Vec<String> {
    backend.calls.iter().filter(|call| !matches!(call.as_str(), "load_runs" | "feature_runs")).cloned().collect()
}

#[test]
fn discarding_the_last_listed_run_selects_the_one_above_it_and_the_next_key_acts_on_that_run() {
    let screen = show(&mut Fake::with(three()), to_runs());
    let mut backend = Fake::with(three());
    let events = [to_runs(), vec![press(KeyCode::Down), press(KeyCode::Down), click(run_button(&screen, "charlie", "[Discard]")), key('y')]].concat();
    let after = show(&mut backend, events.clone());
    assert_eq!(selected(&after).as_deref(), Some("bravo"), "{after:#?}");

    let mut backend = Fake::with(three());
    show(&mut backend, [events, vec![key('c')]].concat());
    assert_eq!(acted(&backend), vec!["discard run-c", "check run-b"]);
}

#[test]
fn discarding_the_only_run_leaves_an_empty_list_whose_keys_act_on_nothing() {
    let only = || vec![run("run-c", "charlie", "blocked")];
    let screen = show(&mut Fake::with(only()), to_runs());
    let mut backend = Fake::with(only());
    let after = show(&mut backend, [to_runs(), vec![click(run_button(&screen, "charlie", "[Discard]")), key('y'), key('c'), key('s'), key('p')]].concat());
    assert!(after.iter().any(|line| line.contains("No runs yet")), "{after:#?}");
    assert_eq!(acted(&backend), vec!["discard run-c"]);
}

#[test]
fn when_the_list_comes_back_in_another_order_the_selection_stays_on_the_same_run() {
    let screen = show(&mut Fake::with(three()), to_runs());
    let mut backend = Fake { reorder: true, ..Fake::with(three()) };
    let after = show(&mut backend, [to_runs(), vec![click(run_button(&screen, "alpha", "[Pause]")), key('s')]].concat());
    assert_eq!(selected(&after).as_deref(), Some("alpha"), "{after:#?}");
    assert!(after.iter().any(|line| line.contains("Stop run-a?")), "{after:#?}");
}

#[test]
fn when_the_runs_cannot_be_read_after_an_action_the_reason_is_shown_and_no_key_acts_on_a_stale_run() {
    let screen = show(&mut Fake::with(three()), to_runs());
    let mut backend = Fake { unreadable_after: true, ..Fake::with(three()) };
    let after = show(&mut backend, [to_runs(), vec![click(run_button(&screen, "bravo", "[Discard]")), key('y'), key('c'), key('s'), key('p')]].concat());
    assert!(after.iter().any(|line| line.contains("the runs could not be read")), "{after:#?}");
    assert_eq!(acted(&backend), vec!["discard run-b"]);
}
