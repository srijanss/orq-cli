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
        self.storage.clone()
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

fn run(backend: &mut Fake, events: Vec<Event>) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let (width, height) = (70u16, 30u16);
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    let _ = run_browser(&mut terminal, &model, backend, &mut Script(events.into()));
    let buffer = terminal.backend().buffer();
    (0..height).map(|y| (0..width).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string()).collect()
}

fn has(lines: &[String], needle: &str) -> bool {
    lines.iter().any(|l| l.contains(needle))
}

#[test]
fn settings_shows_storage_usage_policy_last_cleanup_and_pins() {
    let lines = run(&mut Fake::new(), vec![key(KeyCode::Tab)]);
    for needle in ["Storage", "logs: 3 file(s), 4.0 KiB", "reports: 1 file(s)", "failed runs: 30 days", "2026-11-01T10:00:00Z", "run-7"] {
        assert!(has(&lines, needle), "missing {needle}: {lines:#?}");
    }
}

#[test]
fn storage_is_a_section_of_settings_not_a_fourth_tab() {
    let lines = run(&mut Fake::new(), vec![]);
    assert!(!lines[2].contains("Storage"), "{lines:?}");
}

#[test]
fn a_storage_that_cannot_be_read_says_why_and_the_rest_still_works() {
    let mut fake = Fake::new();
    fake.storage = StorageView { error: Some("permission denied".into()), ..StorageView::default() };
    let lines = run(&mut fake, vec![key(KeyCode::Tab)]);
    assert!(has(&lines, "permission denied"), "{lines:#?}");
    assert!(has(&lines, "Implementer"), "{lines:#?}");
}

#[test]
fn storage_text_from_disk_cannot_inject_terminal_controls() {
    let mut fake = Fake::new();
    fake.storage.pinned = vec!["bad\u{1b}[2Jrun".into()];
    let lines = run(&mut fake, vec![key(KeyCode::Tab)]);
    assert!(!lines.iter().any(|l| l.contains('\u{1b}')), "{lines:#?}");
}
