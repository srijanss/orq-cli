use std::collections::VecDeque;
use std::io;

use orq_tui::{
    run_browser, AgentSetting, BrowserBackend, BrowserEntry, EventSource, LedgerErrorView, PopupModel,
    PromoteOutcome, SettingsView,
};
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Terminal;

struct Script(VecDeque<Event>);

impl EventSource for Script {
    fn next_event(&mut self) -> io::Result<Event> {
        self.0.pop_front().ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "script exhausted"))
    }
}

/// Mimics the real backend: a role must be both-empty or both-filled to save.
struct Fake {
    view: SettingsView,
    saved: Vec<(AgentSetting, AgentSetting)>,
    reject_reviewer: Option<String>,
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
    fn save_settings(&mut self, implementer: &AgentSetting, reviewer: &AgentSetting) -> Result<(), String> {
        for role in [implementer, reviewer] {
            if role.harness.is_empty() != role.model.is_empty() {
                return Err("both harness and model are required".into());
            }
        }
        self.saved.push((implementer.clone(), reviewer.clone()));
        self.view.implementer = implementer.clone();
        self.view.reviewer = reviewer.clone();
        self.view.error = match &self.reject_reviewer {
            Some(model) if &reviewer.model == model => Some(format!("model {model} is not in allowedModels")),
            _ => None,
        };
        Ok(())
    }
}

impl Fake {
    fn empty() -> Fake {
        Fake { view: SettingsView::default(), saved: Vec::new(), reject_reviewer: None }
    }
}

fn setting(harness: &str, model: &str) -> AgentSetting {
    AgentSetting { harness: harness.into(), model: model.into() }
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn typed(text: &str) -> Vec<Event> {
    text.chars().map(|c| key(KeyCode::Char(c))).collect()
}

#[allow(dead_code)]
fn click(column: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

/// Runs the script; returns whether the popup closed itself and the last screen.
fn run(backend: &mut Fake, events: Vec<Event>) -> (bool, Vec<String>) {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let (width, height) = (60u16, 24u16);
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    let closed = run_browser(&mut terminal, &model, backend, &mut Script(events.into())).is_ok();
    let buffer = terminal.backend().buffer();
    let lines = (0..height)
        .map(|y| (0..width).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
        .collect();
    (closed, lines)
}

fn has(lines: &[String], needle: &str) -> bool {
    lines.iter().any(|l| l.contains(needle))
}

#[test]
fn an_unset_role_can_be_filled_one_field_at_a_time() {
    let mut backend = Fake::empty();
    let mut events = vec![key(KeyCode::Tab), key(KeyCode::Enter)];
    events.extend(typed("claude"));
    events.extend([key(KeyCode::Enter), key(KeyCode::Down), key(KeyCode::Enter)]);
    events.extend(typed("opus"));
    events.push(key(KeyCode::Enter));

    let (_, lines) = run(&mut backend, events);

    assert_eq!(backend.saved, vec![(setting("claude", "opus"), setting("", ""))]);
    assert!(has(&lines, "Saved"), "{lines:?}");
}

#[test]
fn a_half_filled_role_keeps_the_draft_and_says_what_is_missing() {
    let mut backend = Fake::empty();
    let mut events = vec![key(KeyCode::Tab), key(KeyCode::Enter)];
    events.extend(typed("claude"));
    events.push(key(KeyCode::Enter));

    let (closed, lines) = run(&mut backend, events);

    assert!(!closed);
    assert!(backend.saved.is_empty());
    assert!(has(&lines, "claude"), "{lines:?}");
    assert!(has(&lines, "model"), "{lines:?}");
    assert!(!has(&lines, "Not saved"), "{lines:?}");
}
