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
            entry("golf", "pending"),
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

/// The first line of a feature's card, which holds its button or its latest run's status: the
/// line above its name.
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

/// The title line of the `nth` Orchestrations card of `feature`, which holds that run's buttons.
fn run_line(screen: &[String], feature: &str, nth: usize) -> (u16, String) {
    let (y, line) = screen
        .iter()
        .enumerate()
        .filter(|(_, line)| line.contains(&format!(" {feature} ")))
        .nth(nth)
        .unwrap_or_else(|| panic!("run {nth} of {feature} is listed: {screen:#?}"));
    (y as u16, line.clone())
}

fn run_button(screen: &[String], feature: &str, nth: usize, button: &str) -> (u16, u16) {
    let (y, line) = run_line(screen, feature, nth);
    let byte = line.find(button).unwrap_or_else(|| panic!("run {nth} of {feature} has {button}: {line:?}"));
    (line[..byte].chars().count() as u16 + 1, y)
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
/// A terminal height at which the Orchestrations tab lists its runs one line each.
const RUNS_COMPACT: u16 = 4;
const RUN_BUTTONS: [&str; 4] = ["[Pause]", "[Stop]", "[Resume]", "[Discard]"];

/// What the fake was asked to do to runs, leaving out the reads.
fn acted(backend: &Fake) -> Vec<String> {
    backend.calls.iter().filter(|call| !matches!(call.as_str(), "load_runs" | "feature_runs" | "profiles")).cloned().collect()
}

#[test]
fn a_feature_row_shows_orchestrate_only_when_pending_with_no_live_run_and_otherwise_its_runs_status() {
    let screen = show(&mut Fake::default(), vec![]);
    for (feature, button, badge) in [
        ("alpha", true, None),
        ("bravo", false, Some("running")),
        ("charlie", false, Some("paused")),
        ("delta", false, Some("failed")),
        ("echo", false, Some("blocked")),
        ("foxtrot", false, Some("completed")),
    ] {
        let (_, line) = button_line(&screen, feature);
        assert_eq!(line.contains("[Orchestrate]"), button, "{feature}: {line:?}");
        assert!(RUN_BUTTONS.iter().all(|label| !line.contains(label)), "{feature} has no run buttons: {line:?}");
        if let Some(badge) = badge {
            assert!(line.contains(badge), "{feature} shows {badge}: {line:?}");
        }
    }
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
fn a_click_elsewhere_cancels_a_run_buttons_question_so_a_later_y_does_nothing() {
    let screen = show(&mut Fake::default(), to_runs());
    let tab = text_at(&screen, "Features");
    let mut backend = Fake::default();
    let after = show(&mut backend, [to_runs(), vec![click(run_button(&screen, "echo", 0, "[Discard]")), click(tab), key('y')]].concat());
    assert!(!backend.calls.iter().any(|call| call.starts_with("discard")), "the question was cancelled: {:?}", backend.calls);
    assert!(!after.iter().any(|line| line.contains("Discard run-5?")), "{after:#?}");
}

#[test]
fn stopping_from_the_orchestrations_tab_reads_the_runs_once_for_the_list_and_the_rows() {
    let mut backend = Fake::default();
    let screen = show(&mut backend, [to_runs(), vec![press(KeyCode::Down), key('s'), key('y')]].concat());
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
fn a_run_button_answered_with_y_reads_the_runs_once_for_the_list_and_the_rows() {
    let screen = show(&mut Fake::default(), to_runs());
    for (feature, nth, button, call) in [("bravo", 1, "[Stop]", "stop run-2"), ("charlie", 0, "[Resume]", "resume run-3"), ("echo", 0, "[Discard]", "discard run-5")] {
        let mut backend = Fake::default();
        show(&mut backend, [to_runs(), vec![click(run_button(&screen, feature, nth, button)), key('y')]].concat());
        let after = backend.calls.iter().skip_while(|c| *c != call).skip(1).filter(|c| *c == "load_runs" || *c == "feature_runs").count();
        assert!(backend.calls.contains(&call.to_string()), "{:?}", backend.calls);
        assert_eq!(after, 1, "{call}: {:?}", backend.calls);
    }
}

#[test]
fn a_refused_resume_check_shows_why_and_asks_nothing_so_y_resumes_nothing() {
    let screen = show(&mut Fake::default(), to_runs());
    let mut backend = Fake { refuse_resume: Some("run-3 cannot resume: its base moved".into()), ..Fake::default() };
    let after = show(&mut backend, [to_runs(), vec![click(run_button(&screen, "charlie", 0, "[Resume]"))]].concat());
    assert!(after.iter().any(|line| line.contains("its base moved")), "{after:#?}");

    let mut backend = Fake { refuse_resume: Some("run-3 cannot resume: its base moved".into()), ..Fake::default() };
    show(&mut backend, [to_runs(), vec![click(run_button(&screen, "charlie", 0, "[Resume]")), key('y')]].concat());
    assert!(!backend.calls.contains(&"resume run-3".to_string()), "nothing was asked, so y resumes nothing: {:?}", backend.calls);
}

#[test]
fn a_failed_stop_resume_or_discard_is_shown_without_its_control_characters_and_the_row_keeps_its_buttons() {
    let screen = show(&mut Fake::default(), to_runs());
    for (feature, nth, button, call) in
        [("bravo", 1, "[Stop]", "stop run-2"), ("charlie", 0, "[Resume]", "resume run-3"), ("echo", 0, "[Discard]", "discard run-5")]
    {
        let mut backend = Fake { fail_with: Some("it failed: \u{1b}[2Jdisk full".into()), ..Fake::default() };
        let after = show(&mut backend, [to_runs(), vec![click(run_button(&screen, feature, nth, button)), key('y')]].concat());
        assert!(backend.calls.contains(&call.to_string()), "{:?}", backend.calls);
        assert!(after.iter().any(|line| line.contains("disk full")), "{feature}: {after:#?}");
        assert!(!after.iter().any(|line| line.contains('\u{1b}')), "{feature}: no raw escape reaches the screen");
        assert!(run_line(&after, feature, nth).1.contains(button), "{feature} still offers {button}: {after:#?}");
    }
}

#[test]
fn a_runs_buttons_and_its_features_status_follow_it_after_a_pause_or_a_discard() {
    let screen = show(&mut Fake::default(), to_runs());

    let paused = show(&mut Fake::default(), [to_runs(), vec![click(run_button(&screen, "bravo", 1, "[Pause]"))]].concat());
    let (_, bravo) = run_line(&paused, "bravo", 1);
    assert!(bravo.contains("[Stop]") && !bravo.contains("[Pause]"), "a pause pending leaves only Stop: {bravo:?}");
    let features = show(&mut Fake::default(), [to_runs(), vec![click(run_button(&screen, "bravo", 1, "[Pause]")), press(KeyCode::Esc)]].concat());
    assert!(button_line(&features, "bravo").1.contains("pause_requested"), "{features:#?}");

    let discarded = show(&mut Fake::default(), [to_runs(), vec![click(run_button(&screen, "echo", 0, "[Discard]")), key('y')]].concat());
    assert!(!discarded.iter().any(|line| line.contains(" echo ")), "a discarded run leaves the list: {discarded:#?}");
}

#[test]
fn a_run_that_paused_on_its_own_shows_on_its_feature_row_after_the_orchestrations_tab_lists_it() {
    let mut backend = Fake { changes: vec![("load_runs", "run-2", "paused")], ..Fake::default() };
    let screen = show(&mut backend, [to_runs(), vec![press(KeyCode::Esc)]].concat());

    let (_, line) = button_line(&screen, "bravo");
    assert!(line.contains("paused") && !line.contains("running"), "{line:?}");
}

#[test]
fn coming_back_to_features_from_another_tab_reads_the_runs_again() {
    let mut backend = Fake { changes: vec![("load_reviews", "run-2", "failed")], ..Fake::default() };
    let screen = show(
        &mut backend,
        vec![press(KeyCode::Tab), press(KeyCode::Tab), press(KeyCode::Tab), press(KeyCode::Tab)],
    );

    let (_, line) = button_line(&screen, "bravo");
    assert!(line.contains("failed") && !line.contains("running"), "{line:?}");
}

#[test]
fn after_a_name_search_orchestrate_opens_the_picker_for_that_row() {
    let search = || vec![key('/'), key('g'), key('o'), key('l'), key('f'), press(KeyCode::Enter)];
    let screen = show(&mut Fake::default(), search());
    assert!(!screen.iter().any(|line| line.contains(" alpha ")), "the search hides alpha: {screen:#?}");

    let mut backend = Fake::default();
    let picked = show(&mut backend, [search(), vec![click(button_at(&screen, "golf", "[Orchestrate]"))]].concat());
    assert!(backend.calls.contains(&"profiles".to_string()), "{:?} {picked:#?}", backend.calls);
}

#[test]
fn after_the_list_scrolls_orchestrate_opens_the_picker_for_that_row() {
    let (width, height) = (WIDTH, 22);
    let down = || vec![press(KeyCode::Down); 7];
    let screen = show_sized(&mut Fake::default(), down(), width, height);
    assert!(!screen.iter().any(|line| line.contains(" alpha ")), "alpha has scrolled away: {screen:#?}");

    let mut backend = Fake::default();
    show_sized(&mut backend, [down(), vec![click(button_at(&screen, "golf", "[Orchestrate]"))]].concat(), width, height);
    assert!(backend.calls.contains(&"profiles".to_string()), "{:?}", backend.calls);
}

#[test]
fn the_compact_layouts_draw_no_row_buttons_and_a_click_there_acts_on_no_run() {
    // Features rows are one line each at 9 rows; the Orchestrations cards need fewer rows still.
    for (tab, height) in [(vec![], 9), (to_runs(), RUNS_COMPACT)] {
        let width = WIDTH;
        let screen = show_sized(&mut Fake::default(), tab.clone(), width, height);
        assert!(screen.iter().any(|line| line.contains("bravo")), "the rows are listed, one line each: {screen:#?}");
        assert!(!screen.iter().any(|line| ALL.iter().any(|label| line.contains(label))), "{screen:#?}");

        let mut backend = Fake::default();
        let clicks: Vec<Event> = (0..height).flat_map(|row| [click((width - 3, row)), key('y')]).collect();
        show_sized(&mut backend, [tab, clicks].concat(), width, height);
        assert!(
            !acted(&backend).iter().any(|call| ["pause", "stop", "resume", "discard", "check"].iter().any(|verb| call.starts_with(verb))),
            "{:?}",
            backend.calls
        );
    }
}
