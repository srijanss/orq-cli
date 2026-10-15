use std::collections::VecDeque;
use std::io;

use orq_tui::{
    run_browser, AgentSetting, BrowserBackend, BrowserEntry, EventSource, LedgerErrorView, PopupModel,
    HarnessOption, PromoteOutcome, SettingsView,
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
    saved: Vec<(AgentSetting, AgentSetting)>,
}

impl BrowserBackend for Fake {
    fn load(&mut self) -> Result<Vec<BrowserEntry>, LedgerErrorView> {
        Ok(vec![])
    }
    fn promote(&mut self, _feature: &str) -> PromoteOutcome {
        PromoteOutcome::Saved
    }
    fn load_settings(&mut self) -> SettingsView {
        self.view.clone()
    }
    fn harness_options(&mut self) -> Vec<HarnessOption> {
        vec![
            HarnessOption { harness: "claude".into(), models: vec!["opus".into(), "sonnet".into()] },
            HarnessOption { harness: "pi".into(), models: vec!["gpt-6".into()] },
        ]
    }
    fn save_settings(&mut self, implementer: &AgentSetting, reviewer: &AgentSetting) -> Result<(), String> {
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

fn fake() -> Fake {
    Fake {
        view: SettingsView {
            implementer: setting("claude", "opus"),
            reviewer: setting("pi", "gpt-6"),
            reviewer_override: None,
            error: None,
        },
        saved: Vec::new(),
    }
}

fn run(backend: &mut Fake, events: Vec<Event>) {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(TestBackend::new(60, 24)).unwrap();
    let _ = run_browser(&mut terminal, &model, backend, &mut Script(events.into()));
}

#[test]
fn down_while_editing_a_harness_picks_the_next_installed_harness() {
    let mut backend = fake();
    run(&mut backend, vec![key(KeyCode::Tab), key(KeyCode::Enter), key(KeyCode::Down), key(KeyCode::Enter)]);

    assert_eq!(backend.saved, vec![(setting("pi", "opus"), setting("pi", "gpt-6"))]);
}

#[test]
fn down_while_editing_a_profile_picks_from_that_harness_profiles() {
    let mut backend = fake();
    let events = vec![key(KeyCode::Tab), key(KeyCode::Down), key(KeyCode::Enter), key(KeyCode::Down), key(KeyCode::Enter)];
    run(&mut backend, events);

    assert_eq!(backend.saved, vec![(setting("claude", "sonnet"), setting("pi", "gpt-6"))]);
}
