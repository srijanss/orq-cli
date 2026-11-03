use std::collections::VecDeque;
use std::io;

use orq_tui::{
    run_browser, AgentSetting, BrowserBackend, BrowserEntry, EventSource, LedgerErrorView, PopupModel,
    PromoteOutcome, SettingsView, StorageView,
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
    view: SettingsView,
    storage: StorageView,
    save_error: Option<String>,
    saved: Vec<(AgentSetting, AgentSetting)>,
    preview: Result<Vec<String>, String>,
    prune: Result<Vec<String>, String>,
    prunes: usize,
    storage_loads: usize,
}

impl Fake {
    fn new() -> Fake {
        Fake {
            view: SettingsView {
                implementer: setting("claude", "opus"),
                reviewer: setting("pi", "gpt-6"),
                reviewer_override: None,
                error: None,
            },
            storage: StorageView {
                error: None,
                usage: vec!["logs: 3 file(s), 4.0 KiB".into(), "reports: 1 file(s), 1.0 KiB".into()],
                policy: vec!["failed runs: 30 days".into(), "logs: 14 days".into()],
                last_cleanup: Some("2026-11-01T10:00:00Z".into()),
                pinned: vec!["run-7".into()],
            },
            save_error: None,
            saved: Vec::new(),
            preview: Ok(vec!["runs/old/events.jsonl (2.0 KiB)".into(), "runs/old/tmp/x (1.0 KiB)".into()]),
            prune: Ok(vec!["Removed 2 file(s), freed 3.0 KiB.".into()]),
            prunes: 0,
            storage_loads: 0,
        }
    }
}

impl BrowserBackend for Fake {
    fn load(&mut self) -> Result<Vec<BrowserEntry>, LedgerErrorView> {
        Ok(vec![BrowserEntry {
            name: "alpha".into(),
            description: None,
            status: "draft".into(),
            depends_on: vec![],
            test_file: None,
        }])
    }
    fn promote(&mut self, _feature: &str) -> PromoteOutcome {
        PromoteOutcome::Saved
    }
    fn load_settings(&mut self) -> SettingsView {
        self.view.clone()
    }
    fn load_storage(&mut self) -> StorageView {
        self.storage_loads += 1;
        self.storage.clone()
    }
    fn preview_storage(&mut self) -> Result<Vec<String>, String> {
        self.preview.clone()
    }
    fn prune_storage(&mut self) -> Result<Vec<String>, String> {
        self.prunes += 1;
        self.prune.clone()
    }
    fn save_settings(&mut self, implementer: &AgentSetting, reviewer: &AgentSetting) -> Result<(), String> {
        if let Some(error) = &self.save_error {
            return Err(error.clone());
        }
        self.saved.push((implementer.clone(), reviewer.clone()));
        self.view.implementer = implementer.clone();
        self.view.reviewer = reviewer.clone();
        Ok(())
    }
}


fn setting(harness: &str, model: &str) -> AgentSetting {
    AgentSetting { harness: harness.into(), model: model.into() }
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn ch(c: char) -> Event {
    key(KeyCode::Char(c))
}

fn run(backend: &mut Fake, events: Vec<Event>) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let (width, height) = (70u16, 32u16);
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    let _ = run_browser(&mut terminal, &model, backend, &mut Script(events.into()));
    let buffer = terminal.backend().buffer();
    (0..height).map(|y| (0..width).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string()).collect()
}

fn has(lines: &[String], needle: &str) -> bool {
    lines.iter().any(|l| l.contains(needle))
}

#[test]
fn p_previews_what_a_cleanup_would_remove_and_removes_nothing() {
    let mut fake = Fake::new();
    let lines = run(&mut fake, vec![key(KeyCode::Tab), ch('p')]);
    assert!(has(&lines, "runs/old/events.jsonl"), "{lines:#?}");
    assert_eq!(fake.prunes, 0);
}

#[test]
fn c_asks_first_and_y_runs_the_cleanup_then_reloads_the_usage() {
    let mut fake = Fake::new();
    let lines = run(&mut fake, vec![key(KeyCode::Tab), ch('c')]);
    assert!(has(&lines, "runs/old/events.jsonl") && has(&lines, "y to confirm"), "{lines:#?}");
    assert_eq!(fake.prunes, 0, "nothing is removed before the confirmation");

    let mut fake = Fake::new();
    let lines = run(&mut fake, vec![key(KeyCode::Tab), ch('c'), ch('y')]);
    assert_eq!(fake.prunes, 1);
    assert!(has(&lines, "Removed 2 file(s), freed 3.0 KiB."), "{lines:#?}");
    assert_eq!(fake.storage_loads, 2, "the usage is read again after the cleanup");
}

#[test]
fn any_other_key_cancels_the_cleanup() {
    for cancel in [key(KeyCode::Esc), ch('n'), key(KeyCode::Enter), ch('x')] {
        let mut fake = Fake::new();
        let lines = run(&mut fake, vec![key(KeyCode::Tab), ch('c'), cancel]);
        assert_eq!(fake.prunes, 0);
        assert!(has(&lines, "Cleanup cancelled"), "{lines:#?}");
    }
}

#[test]
fn y_without_a_pending_confirmation_does_nothing() {
    let mut fake = Fake::new();
    run(&mut fake, vec![key(KeyCode::Tab), ch('y')]);
    assert_eq!(fake.prunes, 0);
}

#[test]
fn nothing_to_clean_is_said_and_no_confirmation_is_asked() {
    let mut fake = Fake::new();
    fake.preview = Ok(vec![]);
    let lines = run(&mut fake, vec![key(KeyCode::Tab), ch('c'), ch('y')]);
    assert!(has(&lines, "Nothing to clean up"), "{lines:#?}");
    assert_eq!(fake.prunes, 0);
}

#[test]
fn failures_of_the_preview_and_of_the_cleanup_are_shown() {
    let mut fake = Fake::new();
    fake.preview = Err("permission denied".into());
    let lines = run(&mut fake, vec![key(KeyCode::Tab), ch('c')]);
    assert!(has(&lines, "permission denied"), "{lines:#?}");

    let mut fake = Fake::new();
    fake.prune = Err("disk is read-only".into());
    let lines = run(&mut fake, vec![key(KeyCode::Tab), ch('c'), ch('y')]);
    assert!(has(&lines, "disk is read-only"), "{lines:#?}");
}

#[test]
fn a_long_preview_is_clipped_to_the_screen() {
    let mut fake = Fake::new();
    fake.preview = Ok((0..200).map(|i| format!("runs/r/file-{i}")).collect());
    let lines = run(&mut fake, vec![key(KeyCode::Tab), ch('c')]);
    assert_eq!(lines.len(), 32);
    assert!(has(&lines, "more"), "{lines:#?}");
    assert!(has(&lines, "y to confirm"), "the prompt must stay visible: {lines:#?}");
}
