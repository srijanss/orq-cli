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
            if role.harness.is_empty() != role.profile.is_empty() {
                return Err("both harness and profile are required".into());
            }
        }
        self.saved.push((implementer.clone(), reviewer.clone()));
        self.view.implementer = implementer.clone();
        self.view.reviewer = reviewer.clone();
        self.view.error = match &self.reject_reviewer {
            Some(model) if &reviewer.profile == model => Some(format!("model {model} is not in allowedModels")),
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

fn setting(harness: &str, profile: &str) -> AgentSetting {
    AgentSetting { harness: harness.into(), profile: profile.into() }
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
fn the_saved_view_is_reloaded_so_validation_errors_appear() {
    let mut backend = Fake::empty();
    backend.view.reviewer = setting("pi", "ok");
    backend.reject_reviewer = Some("gpt-9".into());
    let mut events = vec![key(KeyCode::Tab), key(KeyCode::Down), key(KeyCode::Down), key(KeyCode::Down)];
    events.push(key(KeyCode::Enter));
    events.extend((0..2).map(|_| key(KeyCode::Backspace)));
    events.extend(typed("gpt-9"));
    events.push(key(KeyCode::Enter));

    let (_, lines) = run(&mut backend, events);

    assert!(has(&lines, "allowedModels"), "{lines:?}");
}
