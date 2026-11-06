//! The launch picker opens each role's step on the profile configured for that role, so Enter
//! accepts it; the choice can still be changed.

use std::collections::VecDeque;
use std::io;

use orq_tui::{run_browser, BrowserBackend, BrowserEntry, EventSource, LaunchDefaults, LedgerErrorView, PopupModel, PromoteOutcome};
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
    defaults: LaunchDefaults,
    catalog: Vec<String>,
    launched: Vec<(String, String)>,
    saved_servers: Vec<Vec<String>>,
}

impl BrowserBackend for Fake {
    fn load(&mut self) -> Result<Vec<BrowserEntry>, LedgerErrorView> {
        Ok(vec![BrowserEntry { name: "alpha".into(), description: None, status: "pending".into(), depends_on: vec![], test_file: None }])
    }
    fn promote(&mut self, _feature: &str) -> PromoteOutcome {
        PromoteOutcome::Saved
    }
    fn mcp_catalog(&mut self) -> Result<Vec<String>, String> {
        Ok(self.catalog.clone())
    }
    fn save_mcp_servers(&mut self, servers: &[String]) -> Result<(), String> {
        self.saved_servers.push(servers.to_vec());
        Ok(())
    }
    fn launch_profiles(&mut self) -> Result<Vec<String>, String> {
        Ok(vec!["opus".into(), "sonnet".into(), "kimi".into()])
    }
    fn launch_defaults(&mut self) -> LaunchDefaults {
        self.defaults.clone()
    }
    fn launch(&mut self, _feature: &str, implementer: &str, reviewer: &str) -> Result<String, String> {
        self.launched.push((implementer.into(), reviewer.into()));
        Ok("run-7".into())
    }
}

fn fake(implementer: Option<&str>, reviewer: Option<&str>) -> Fake {
    Fake {
        defaults: LaunchDefaults { implementer: implementer.map(String::from), reviewer: reviewer.map(String::from) },
        catalog: vec![],
        launched: vec![],
        saved_servers: vec![],
    }
}

fn press(backend: &mut Fake, codes: &[KeyCode]) {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let mut events: VecDeque<Event> = VecDeque::from([KeyCode::Down, KeyCode::Char('L')].map(|code| Event::Key(KeyEvent::new(code, KeyModifiers::NONE))));
    events.extend(codes.iter().map(|code| Event::Key(KeyEvent::new(*code, KeyModifiers::NONE))));
    let _ = run_browser(&mut terminal, &model, backend, &mut Script(events));
}

fn pair(implementer: &str, reviewer: &str) -> Vec<(String, String)> {
    vec![(implementer.to_string(), reviewer.to_string())]
}

#[test]
fn enter_at_each_step_launches_the_configured_implementer_and_reviewer() {
    let mut backend = fake(Some("sonnet"), Some("kimi"));

    press(&mut backend, &[KeyCode::Enter, KeyCode::Enter]);

    assert_eq!(backend.launched, pair("sonnet", "kimi"));
}

#[test]
fn the_arrows_still_choose_another_profile_starting_from_the_configured_one() {
    let mut backend = fake(Some("sonnet"), Some("kimi"));

    press(&mut backend, &[KeyCode::Up, KeyCode::Enter, KeyCode::Up, KeyCode::Enter]);

    assert_eq!(backend.launched, pair("opus", "sonnet"));
}

#[test]
fn a_role_with_no_default_or_one_not_offered_starts_on_the_first_profile() {
    let mut backend = fake(None, Some("not-a-profile"));

    press(&mut backend, &[KeyCode::Enter, KeyCode::Enter]);

    assert_eq!(backend.launched, pair("opus", "opus"));
}

#[test]
fn the_server_step_after_the_reviewer_still_starts_on_the_first_server() {
    let mut backend = fake(Some("sonnet"), Some("kimi"));
    backend.catalog = vec!["one".into(), "two".into(), "three".into()];

    press(&mut backend, &[KeyCode::Enter, KeyCode::Enter, KeyCode::Char(' '), KeyCode::Enter]);

    assert_eq!(backend.launched, pair("sonnet", "kimi"));
    assert_eq!(backend.saved_servers, vec![vec!["one".to_string()]]);
}
