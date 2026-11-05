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
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).unwrap();
    let _ = run_browser(&mut terminal, &model, backend, &mut Script(events.into()));
    let buffer = terminal.backend().buffer();
    (0..HEIGHT).map(|y| (0..WIDTH).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>()).collect()
}

/// The card line that holds a feature's buttons: the line above its name.
fn button_line(screen: &[String], feature: &str) -> (u16, String) {
    let y = screen
        .iter()
        .position(|line| line.contains(&format!(" {feature} ")))
        .unwrap_or_else(|| panic!("{feature} is listed: {screen:#?}"));
    (y as u16 - 1, screen[y - 1].clone())
}

/// Where the label `button` of `feature`'s row is drawn.
fn button_at(screen: &[String], feature: &str, button: &str) -> (u16, u16) {
    let (y, line) = button_line(screen, feature);
    let byte = line.find(button).unwrap_or_else(|| panic!("{feature} has {button}: {line:?}"));
    (line[..byte].chars().count() as u16 + 1, y)
}

fn click((column, row): (u16, u16)) -> Event {
    Event::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column, row, modifiers: KeyModifiers::NONE })
}

fn key(c: char) -> Event {
    Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
}

const ALL: [&str; 5] = ["[Orchestrate]", "[Pause]", "[Stop]", "[Resume]", "[Discard]"];

#[test]
fn each_feature_row_shows_the_buttons_its_latest_run_allows() {
    let screen = show(&mut Fake::default(), vec![]);
    for (feature, expected) in [
        ("alpha", &["[Orchestrate]"][..]),
        ("bravo", &["[Pause]", "[Stop]"][..]),
        ("charlie", &["[Resume]"][..]),
        ("delta", &["[Resume]", "[Discard]"][..]),
        ("echo", &["[Discard]"][..]),
        ("foxtrot", &[][..]),
    ] {
        let (_, line) = button_line(&screen, feature);
        for label in ALL {
            assert_eq!(line.contains(label), expected.contains(&label), "{feature} and {label}: {line:?}");
        }
    }
}

#[test]
fn a_row_button_acts_on_its_own_rows_run_and_asks_before_stopping_resuming_or_discarding() {
    let screen = show(&mut Fake::default(), vec![]);

    let mut backend = Fake::default();
    show(&mut backend, vec![click(button_at(&screen, "bravo", "[Pause]"))]);
    assert!(backend.calls.contains(&"pause run-2".to_string()), "{:?}", backend.calls);

    let mut backend = Fake::default();
    show(&mut backend, vec![click(button_at(&screen, "bravo", "[Stop]")), key('y')]);
    assert!(backend.calls.contains(&"stop run-2".to_string()), "{:?}", backend.calls);

    let mut backend = Fake::default();
    show(&mut backend, vec![click(button_at(&screen, "charlie", "[Resume]")), key('y')]);
    assert!(
        backend.calls.contains(&"check run-3".to_string()) && backend.calls.contains(&"resume run-3".to_string()),
        "{:?}",
        backend.calls
    );

    let mut backend = Fake::default();
    let asked = show(&mut backend, vec![click(button_at(&screen, "echo", "[Discard]"))]);
    assert!(asked.iter().any(|line| line.contains("Discard run-5?")), "{asked:#?}");
    assert!(!backend.calls.iter().any(|call| call.starts_with("discard")), "nothing is discarded before y: {:?}", backend.calls);

    let mut backend = Fake::default();
    let done = show(&mut backend, vec![click(button_at(&screen, "echo", "[Discard]")), key('y')]);
    assert!(backend.calls.contains(&"discard run-5".to_string()), "{:?}", backend.calls);
    assert!(done.iter().any(|line| line.contains("Discarded run-5")), "{done:#?}");
    let reloads = backend.calls.iter().filter(|call| *call == "feature_runs").count();
    assert_eq!(reloads, 2, "the rows' runs are read again after the discard: {:?}", backend.calls);

    let mut backend = Fake::default();
    show(&mut backend, vec![click(button_at(&screen, "delta", "[Discard]")), key('n')]);
    assert!(!backend.calls.iter().any(|call| call.starts_with("discard")), "any other key cancels: {:?}", backend.calls);
}

#[test]
fn orchestrate_on_a_row_opens_the_launch_picker_for_that_feature_and_stays_on_features() {
    let screen = show(&mut Fake::default(), vec![]);
    let mut backend = Fake::default();
    let picked = show(&mut backend, vec![click(button_at(&screen, "alpha", "[Orchestrate]"))]);
    assert!(backend.calls.contains(&"profiles".to_string()), "{:?}", backend.calls);
    assert!(picked.iter().any(|line| line.contains("fast")), "{picked:#?}");
}

/// Where `text` is first drawn on `screen`.
fn text_at(screen: &[String], text: &str) -> (u16, u16) {
    screen
        .iter()
        .enumerate()
        .find_map(|(y, line)| line.find(text).map(|byte| (line[..byte].chars().count() as u16 + 1, y as u16)))
        .unwrap_or_else(|| panic!("{text} is drawn: {screen:#?}"))
}

#[test]
fn a_click_elsewhere_cancels_a_row_buttons_question_so_a_later_y_does_nothing() {
    let screen = show(&mut Fake::default(), vec![]);
    let tab = text_at(&screen, "Orchestrations");
    let mut backend = Fake::default();
    let after = show(&mut backend, vec![click(button_at(&screen, "echo", "[Discard]")), click(tab), key('y')]);
    assert!(!backend.calls.iter().any(|call| call.starts_with("discard")), "the question was cancelled: {:?}", backend.calls);
    assert!(!after.iter().any(|line| line.contains("Discard run-5?")), "{after:#?}");
}

fn press(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

#[test]
fn stopping_from_the_orchestrations_tab_reads_the_runs_once_for_the_list_and_the_rows() {
    let mut backend = Fake::default();
    let screen = show(
        &mut backend,
        vec![press(KeyCode::Tab), press(KeyCode::Tab), press(KeyCode::Down), key('s'), key('y')],
    );
    let after_stop = backend
        .calls
        .iter()
        .skip_while(|call| *call != "stop run-2")
        .skip(1)
        .filter(|call| *call == "load_runs" || *call == "feature_runs")
        .count();
    assert!(backend.calls.contains(&"stop run-2".to_string()), "{:?} {screen:#?}", backend.calls);
    assert_eq!(after_stop, 1, "one read of the runs serves both: {:?}", backend.calls);
}

#[test]
fn a_refused_resume_check_shows_why_and_asks_nothing_so_y_resumes_nothing() {
    let screen = show(&mut Fake::default(), vec![]);
    let mut backend = Fake { refuse_resume: Some("run-3 cannot resume: its base moved".into()), ..Fake::default() };
    let after = show(&mut backend, vec![click(button_at(&screen, "charlie", "[Resume]")), key('y')]);
    assert!(!backend.calls.contains(&"resume run-3".to_string()), "{:?}", backend.calls);
    assert!(after.iter().any(|line| line.contains("its base moved")), "{after:#?}");
}

#[test]
fn a_failed_stop_resume_or_discard_is_shown_without_its_control_characters_and_the_row_keeps_its_buttons() {
    let screen = show(&mut Fake::default(), vec![]);
    for (feature, button, call) in
        [("bravo", "[Stop]", "stop run-2"), ("charlie", "[Resume]", "resume run-3"), ("echo", "[Discard]", "discard run-5")]
    {
        let mut backend = Fake { fail_with: Some("it failed: \u{1b}[2Jdisk full".into()), ..Fake::default() };
        let after = show(&mut backend, vec![click(button_at(&screen, feature, button)), key('y')]);
        assert!(backend.calls.contains(&call.to_string()), "{:?}", backend.calls);
        assert!(after.iter().any(|line| line.contains("disk full")), "{feature}: {after:#?}");
        assert!(!after.iter().any(|line| line.contains('\u{1b}')), "{feature}: no raw escape reaches the screen");
        assert!(button_line(&after, feature).1.contains(button), "{feature} still offers {button}: {after:#?}");
    }
}

#[test]
fn a_rows_buttons_follow_its_run_after_a_pause_or_a_discard() {
    let screen = show(&mut Fake::default(), vec![]);

    let paused = show(&mut Fake::default(), vec![click(button_at(&screen, "bravo", "[Pause]"))]);
    let (_, bravo) = button_line(&paused, "bravo");
    assert!(bravo.contains("[Stop]") && !bravo.contains("[Pause]"), "a pause pending leaves only Stop: {bravo:?}");

    let discarded = show(&mut Fake::default(), vec![click(button_at(&screen, "echo", "[Discard]")), key('y')]);
    let (_, echo) = button_line(&discarded, "echo");
    assert!(ALL.iter().all(|label| !echo.contains(label)), "a discarded run leaves no button: {echo:?}");
}

#[test]
fn a_run_that_paused_on_its_own_shows_resume_on_its_row_after_the_orchestrations_tab_lists_it() {
    let mut backend = Fake { changes: vec![("load_runs", "run-2", "paused")], ..Fake::default() };
    let screen = show(&mut backend, vec![press(KeyCode::Tab), press(KeyCode::Tab), press(KeyCode::Esc)]);

    let (_, line) = button_line(&screen, "bravo");
    assert!(line.contains("[Resume]") && !line.contains("[Pause]") && !line.contains("[Stop]"), "{line:?}");
}

#[test]
fn coming_back_to_features_from_another_tab_reads_the_runs_again() {
    let mut backend = Fake { changes: vec![("load_reviews", "run-2", "failed")], ..Fake::default() };
    let screen = show(
        &mut backend,
        vec![press(KeyCode::Tab), press(KeyCode::Tab), press(KeyCode::Tab), press(KeyCode::Tab)],
    );

    let (_, line) = button_line(&screen, "bravo");
    assert!(line.contains("[Resume]") && line.contains("[Discard]") && !line.contains("[Pause]"), "{line:?}");
}
