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

struct Fake {
    view: SettingsView,
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

fn typed(text: &str) -> Vec<Event> {
    text.chars().map(|c| key(KeyCode::Char(c))).collect()
}

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
fn the_tab_bar_offers_features_and_settings() {
    let (_, lines) = run(&mut Fake::new(), vec![]);

    assert!(lines[2].contains("Features") && lines[2].contains("Settings"), "{lines:?}");
}

#[test]
fn tab_opens_settings_showing_both_roles() {
    let (_, lines) = run(&mut Fake::new(), vec![key(KeyCode::Tab)]);

    assert!(has(&lines, "Implementer"), "{lines:?}");
    assert!(has(&lines, "Reviewer"), "{lines:?}");
    assert!(has(&lines, "claude") && has(&lines, "opus"), "{lines:?}");
    assert!(has(&lines, "pi") && has(&lines, "gpt-6"), "{lines:?}");
    assert!(!has(&lines, "alpha"), "{lines:?}");
}

#[test]
fn clicking_the_settings_tab_opens_it_and_tab_returns_to_features() {
    let (_, lines) = run(&mut Fake::new(), vec![click(14, 2)]);
    assert!(has(&lines, "Implementer"), "{lines:?}");

    let (_, lines) = run(&mut Fake::new(), vec![key(KeyCode::Tab), key(KeyCode::Tab)]);
    assert!(has(&lines, "alpha"), "{lines:?}");
}

#[test]
fn esc_leaves_settings_for_features_without_closing() {
    let (closed, lines) = run(&mut Fake::new(), vec![key(KeyCode::Tab), key(KeyCode::Esc)]);

    assert!(!closed, "script should be exhausted, not the popup closed");
    assert!(has(&lines, "alpha"), "{lines:?}");
}

#[test]
fn the_keyboard_edits_a_field_and_enter_saves_it() {
    let mut backend = Fake::new();
    let mut events = vec![key(KeyCode::Tab), key(KeyCode::Enter)];
    events.extend((0..6).map(|_| key(KeyCode::Backspace)));
    events.extend(typed("pi"));
    events.push(key(KeyCode::Enter));

    let (_, lines) = run(&mut backend, events);

    assert_eq!(backend.saved, vec![(setting("pi", "opus"), setting("pi", "gpt-6"))]);
    assert!(has(&lines, "Saved"), "{lines:?}");
}

#[test]
fn arrow_keys_move_between_fields() {
    let mut backend = Fake::new();
    let mut events = vec![key(KeyCode::Tab), key(KeyCode::Down), key(KeyCode::Down), key(KeyCode::Down)];
    events.push(key(KeyCode::Enter));
    events.extend((0..5).map(|_| key(KeyCode::Backspace)));
    events.extend(typed("sonnet"));
    events.push(key(KeyCode::Enter));

    run(&mut backend, events);

    assert_eq!(backend.saved, vec![(setting("claude", "opus"), setting("pi", "sonnet"))]);
}

#[test]
fn the_mouse_picks_a_field_to_edit() {
    let mut backend = Fake::new();
    // Row 7 is the implementer's model line (body starts at row 5).
    let mut events = vec![key(KeyCode::Tab), click(10, 7)];
    events.extend((0..4).map(|_| key(KeyCode::Backspace)));
    events.extend(typed("haiku"));
    events.push(key(KeyCode::Enter));

    run(&mut backend, events);

    assert_eq!(backend.saved, vec![(setting("claude", "haiku"), setting("pi", "gpt-6"))]);
}

#[test]
fn esc_cancels_an_edit_without_saving() {
    let mut backend = Fake::new();
    let mut events = vec![key(KeyCode::Tab), key(KeyCode::Enter)];
    events.extend(typed("xyz"));
    events.push(key(KeyCode::Esc));

    let (closed, lines) = run(&mut backend, events);

    assert!(!closed);
    assert!(backend.saved.is_empty());
    assert!(has(&lines, "claude") && !has(&lines, "claudexyz"), "{lines:?}");
}

#[test]
fn a_failed_save_is_reported_inline_and_keeps_the_edit() {
    let mut backend = Fake::new();
    backend.save_error = Some("settings.toml changed while it was being edited".into());
    let mut events = vec![key(KeyCode::Tab), key(KeyCode::Enter)];
    events.extend(typed("2"));
    events.push(key(KeyCode::Enter));

    let (closed, lines) = run(&mut backend, events);

    assert!(!closed);
    assert!(has(&lines, "changed while it was being edited"), "{lines:?}");
    assert!(has(&lines, "claude2"), "{lines:?}");
}

#[test]
fn a_reviewer_overridden_by_tdd_config_is_flagged() {
    let mut backend = Fake::new();
    backend.view.reviewer_override = Some("pi / openai/gpt-6-astra".into());

    let (_, lines) = run(&mut backend, vec![key(KeyCode::Tab)]);

    assert!(has(&lines, ".tdd-config.json"), "{lines:?}");
    assert!(has(&lines, "openai/gpt-6-astra"), "{lines:?}");
}

#[test]
fn a_settings_file_problem_is_shown_and_not_hidden() {
    let mut backend = Fake::new();
    backend.view.error = Some("settings.toml is not valid TOML".into());

    let (_, lines) = run(&mut backend, vec![key(KeyCode::Tab)]);

    assert!(has(&lines, "not valid TOML"), "{lines:?}");
}
