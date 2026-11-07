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
    /// The runs as the fake's store holds them; `None` until first read, then `runs()`.
    store: Option<Vec<RunRow>>,
    /// Why a resume check refuses, when it does.
    refuse_resume: Option<String>,
    /// What stop, resume and discard fail with, when they do.
    fail_with: Option<String>,
    /// A run's status changing on its own once a read happens: (read, run, status).
    changes: Vec<(&'static str, &'static str, &'static str)>,
}

impl Fake {
    fn store(&mut self) -> &mut Vec<RunRow> {
        self.store.get_or_insert_with(runs)
    }
    fn set_status(&mut self, run_id: &str, status: &str) {
        if let Some(run) = self.store().iter_mut().find(|run| run.run_id == run_id) {
            run.status = status.into();
        }
    }
    /// The process behind a run moved on while `read` was being done.
    fn happen(&mut self, read: &str) {
        let due: Vec<_> = self.changes.iter().filter(|(on, _, _)| *on == read).map(|&(_, id, status)| (id, status)).collect();
        self.changes.retain(|(on, _, _)| *on != read);
        for (run_id, status) in due {
            self.set_status(run_id, status);
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
        reason: None,
    }
}

/// The runs the fake lists, oldest first.
fn runs() -> Vec<RunRow> {
    vec![
        // An older failed run of bravo is superseded by its latest one, which is running.
        run("run-1", "bravo", "failed"),
        run("run-2", "bravo", "running"),
        run("run-3", "charlie", "paused"),
        run("run-4", "delta", "failed"),
        run("run-5", "echo", "blocked"),
        run("run-6", "foxtrot", "completed"),
    ]
}

impl BrowserBackend for Fake {
    fn load(&mut self) -> Result<Vec<BrowserEntry>, LedgerErrorView> {
        Ok(vec![
            entry("alpha", "pending"),
            entry("bravo", "in_progress"),
            entry("charlie", "in_progress"),
            entry("delta", "in_progress"),
            entry("echo", "in_progress"),
            entry("foxtrot", "completed"),
        ])
    }
    fn promote(&mut self, _feature: &str) -> PromoteOutcome {
        PromoteOutcome::Saved
    }
    fn feature_runs(&mut self) -> Vec<RunRow> {
        self.calls.push("feature_runs".into());
        self.store().clone()
    }
    fn load_runs(&mut self) -> Result<Vec<RunRow>, String> {
        self.calls.push("load_runs".into());
        self.happen("load_runs");
        Ok(self.store().clone())
    }
    fn load_reviews(&mut self) -> Result<Vec<orq_tui::ReviewRow>, String> {
        self.happen("load_reviews");
        Ok(vec![])
    }

    fn pause_run(&mut self, run_id: &str) -> Result<String, String> {
        self.calls.push(format!("pause {run_id}"));
        self.set_status(run_id, "pause_requested");
        Ok(format!("Pause requested for {run_id}"))
    }
    fn stop_run(&mut self, run_id: &str) -> Result<String, String> {
        self.calls.push(format!("stop {run_id}"));
        if let Some(why) = self.fail_with.clone() {
            return Err(why);
        }
        Ok(format!("Stopped {run_id}"))
    }
    fn resume_check(&mut self, run_id: &str) -> Result<String, String> {
        self.calls.push(format!("check {run_id}"));
        if let Some(why) = self.refuse_resume.clone() {
            return Err(why);
        }
        Ok("where it stopped".into())
    }
    fn resume_run(&mut self, run_id: &str) -> Result<String, String> {
        self.calls.push(format!("resume {run_id}"));
        if let Some(why) = self.fail_with.clone() {
            return Err(why);
        }
        Ok(format!("Resumed {run_id}"))
    }
    fn discard_run(&mut self, run_id: &str) -> Result<String, String> {
        self.calls.push(format!("discard {run_id}"));
        if let Some(why) = self.fail_with.clone() {
            return Err(why);
        }
        self.store().retain(|run| run.run_id != run_id);
        Ok(format!("Discarded {run_id}"))
    }
    fn launch_profiles(&mut self) -> Result<Vec<String>, String> {
        self.calls.push("profiles".into());
        Ok(vec!["fast".into()])
    }
}

fn show(backend: &mut Fake, events: Vec<Event>) -> Vec<String> {
    show_sized(backend, events, WIDTH, HEIGHT)
}

fn show_sized(backend: &mut Fake, events: Vec<Event>, width: u16, height: u16) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    let _ = run_browser(&mut terminal, &model, backend, &mut Script(events.into()));
    let buffer = terminal.backend().buffer();
    (0..height).map(|y| (0..width).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>()).collect()
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

/// The keys that open the Orchestrations tab from Features.
fn to_runs() -> Vec<Event> {
    vec![press(KeyCode::Tab), press(KeyCode::Tab)]
}

const ALL: [&str; 5] = ["[Orchestrate]", "[Pause]", "[Stop]", "[Resume]", "[Discard]"];

/// The title line of the `nth` run card of `feature` (0 is the first listed): it holds the buttons.
fn run_line(screen: &[String], feature: &str, nth: usize) -> (u16, String) {
    let (y, line) = screen
        .iter()
        .enumerate()
        .filter(|(_, line)| line.contains(&format!(" {feature} ")))
        .nth(nth)
        .unwrap_or_else(|| panic!("run {nth} of {feature} is listed: {screen:#?}"));
    (y as u16, line.clone())
}

/// Where the label `button` on the `nth` run card of `feature` is drawn.
fn run_button(screen: &[String], feature: &str, nth: usize, button: &str) -> (u16, u16) {
    let (y, line) = run_line(screen, feature, nth);
    let byte = line.find(button).unwrap_or_else(|| panic!("run {nth} of {feature} has {button}: {line:?}"));
    (line[..byte].chars().count() as u16 + 1, y)
}

/// What the fake was asked to do to runs, leaving out the reads.
fn acted(backend: &Fake) -> Vec<String> {
    backend.calls.iter().filter(|call| !matches!(call.as_str(), "load_runs" | "feature_runs" | "profiles")).cloned().collect()
}

#[test]
fn each_orchestrations_row_shows_the_buttons_its_runs_status_allows() {
    let screen = show(&mut Fake::default(), to_runs());
    for (feature, nth, expected) in [
        ("bravo", 0, &["[Resume]", "[Discard]"][..]),
        ("bravo", 1, &["[Pause]", "[Stop]"][..]),
        ("charlie", 0, &["[Resume]", "[Discard]"][..]),
        ("delta", 0, &["[Resume]", "[Discard]"][..]),
        ("echo", 0, &["[Discard]"][..]),
        ("foxtrot", 0, &[][..]),
    ] {
        let (_, line) = run_line(&screen, feature, nth);
        for label in ALL {
            assert_eq!(line.contains(label), expected.contains(&label), "{feature} #{nth} and {label}: {line:?}");
        }
    }
    assert!(!screen.iter().any(|line| line.contains("Selected run")), "the buttons are on the rows, not a panel: {screen:#?}");
}

#[test]
fn an_orchestrations_rows_button_acts_on_that_run_and_asks_before_stopping_resuming_or_discarding() {
    let screen = show(&mut Fake::default(), to_runs());
    let on = |events: Vec<Event>| {
        let mut backend = Fake::default();
        let after = show(&mut backend, [to_runs(), events].concat());
        (backend, after)
    };

    let (backend, _) = on(vec![click(run_button(&screen, "bravo", 1, "[Pause]"))]);
    assert_eq!(acted(&backend), vec!["pause run-2"]);

    let (backend, asked) = on(vec![click(run_button(&screen, "bravo", 1, "[Stop]"))]);
    assert!(acted(&backend).is_empty() && asked.iter().any(|line| line.contains("Stop run-2?")), "{asked:#?}");
    let (backend, _) = on(vec![click(run_button(&screen, "bravo", 1, "[Stop]")), key('y')]);
    assert_eq!(acted(&backend), vec!["stop run-2"]);

    let (backend, _) = on(vec![click(run_button(&screen, "charlie", 0, "[Resume]")), key('y')]);
    assert_eq!(acted(&backend), vec!["check run-3", "resume run-3"]);

    let (backend, asked) = on(vec![click(run_button(&screen, "echo", 0, "[Discard]"))]);
    assert!(acted(&backend).is_empty() && asked.iter().any(|line| line.contains("Discard run-5?")), "{asked:#?}");
    let (backend, done) = on(vec![click(run_button(&screen, "echo", 0, "[Discard]")), key('y')]);
    assert_eq!(acted(&backend), vec!["discard run-5"]);
    assert!(done.iter().any(|line| line.contains("Discarded run-5")), "{done:#?}");
    assert!(!done.iter().any(|line| line.contains(" echo ")), "the list is read again without it: {done:#?}");

    let (backend, _) = on(vec![click(run_button(&screen, "bravo", 0, "[Discard]")), key('y')]);
    assert_eq!(acted(&backend), vec!["discard run-1"], "the older run of the same feature is the one discarded");

    let (backend, _) = on(vec![click(run_button(&screen, "delta", 0, "[Discard]")), key('n')]);
    assert!(acted(&backend).is_empty(), "any other key cancels: {:?}", backend.calls);
}

#[test]
fn after_the_orchestrations_list_scrolls_a_rows_button_acts_on_that_rows_run() {
    let (width, height) = (WIDTH, 22);
    let down = || [to_runs(), vec![press(KeyCode::Down); 5]].concat();
    let screen = show_sized(&mut Fake::default(), down(), width, height);
    assert!(!screen.iter().any(|line| line.contains(" bravo ")), "bravo's runs have scrolled away: {screen:#?}");

    let mut backend = Fake::default();
    show_sized(&mut backend, [down(), vec![click(run_button(&screen, "echo", 0, "[Discard]")), key('y')]].concat(), width, height);
    assert_eq!(acted(&backend), vec!["discard run-5"]);
}

#[test]
fn a_feature_row_offers_only_orchestrate_and_shows_its_latest_runs_status() {
    let screen = show(&mut Fake::default(), vec![]);
    // A feature card's first line holds its buttons or its latest run's status; the name is beneath.
    let first_line = |feature: &str| {
        let y = screen.iter().position(|line| line.contains(&format!(" {feature} "))).unwrap_or_else(|| panic!("{feature}: {screen:#?}"));
        screen[y - 1].clone()
    };
    for (feature, badge) in [("bravo", Some("running")), ("charlie", Some("paused")), ("delta", Some("failed")), ("echo", Some("blocked")), ("alpha", None)] {
        let line = first_line(feature);
        for label in ALL {
            assert_eq!(line.contains(label), feature == "alpha" && label == "[Orchestrate]", "{feature} and {label}: {line:?}");
        }
        if let Some(badge) = badge {
            assert!(line.contains(badge), "{feature} shows its run is {badge}: {line:?}");
        }
    }
}

#[test]
fn discard_keep_a_paused_rows_discard_asks_first_and_says_uncommitted_work_is_kept() {
    let screen = show(&mut Fake::default(), to_runs());
    let mut backend = Fake::default();
    let asked = show(&mut backend, [to_runs(), vec![click(run_button(&screen, "charlie", 0, "[Discard]"))]].concat());
    assert!(acted(&backend).is_empty(), "{:?}", backend.calls);
    let prompt = asked.join("\n");
    assert!(prompt.contains("Discard run-3?") && prompt.contains("uncommitted work is kept"), "{asked:#?}");

    let mut backend = Fake::default();
    show(&mut backend, [to_runs(), vec![click(run_button(&screen, "charlie", 0, "[Discard]")), key('y')]].concat());
    assert_eq!(acted(&backend), vec!["discard run-3"]);
}
