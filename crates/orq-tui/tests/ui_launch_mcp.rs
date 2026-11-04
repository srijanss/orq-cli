//! After the reviewer, the launch picker offers the MCP servers of the catalog; the chosen ones are saved
//! before the run starts.

use std::collections::VecDeque;
use std::io;

use orq_tui::{run_browser, BrowserBackend, BrowserEntry, EventSource, LedgerErrorView, PopupModel, PromoteOutcome};
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
    catalog: Result<Vec<String>, String>,
    saved: Vec<Vec<String>>,
    save_result: Result<(), String>,
    launched: Vec<(String, String, String)>,
    /// Every save and launch in the order it happened.
    log: Vec<&'static str>,
}

impl BrowserBackend for Fake {
    fn load(&mut self) -> Result<Vec<BrowserEntry>, LedgerErrorView> {
        let entry = |name: &str| BrowserEntry {
            name: name.into(),
            description: None,
            status: "pending".into(),
            depends_on: vec![],
            test_file: None,
        };
        Ok(vec![entry("alpha"), entry("beta")])
    }
    fn promote(&mut self, _feature: &str) -> PromoteOutcome {
        PromoteOutcome::Saved
    }
    fn launch_profiles(&mut self) -> Result<Vec<String>, String> {
        Ok(vec!["opus".into(), "sonnet".into()])
    }
    fn mcp_catalog(&mut self) -> Result<Vec<String>, String> {
        self.catalog.clone()
    }
    fn save_mcp_servers(&mut self, servers: &[String]) -> Result<(), String> {
        self.log.push("save");
        self.saved.push(servers.to_vec());
        self.save_result.clone()
    }
    fn launch(&mut self, feature: &str, implementer: &str, reviewer: &str) -> Result<String, String> {
        self.log.push("launch");
        self.launched.push((feature.into(), implementer.into(), reviewer.into()));
        Ok("run-7".into())
    }
}

fn fake(catalog: &[&str]) -> Fake {
    Fake {
        catalog: Ok(catalog.iter().map(|s| s.to_string()).collect()),
        saved: vec![],
        save_result: Ok(()),
        launched: vec![],
        log: vec![],
    }
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn show(backend: &mut Fake, events: Vec<Event>) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let _ = run_browser(&mut terminal, &model, backend, &mut Script(events.into()));
    let buffer = terminal.backend().buffer();
    (0..24u16)
        .map(|y| (0..80u16).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
        .collect()
}

fn has(lines: &[String], needle: &str) -> bool {
    lines.iter().any(|l| l.contains(needle))
}

/// `L`, then Enter on opus, then Enter on sonnet: the picker is now on the MCP step.
fn to_mcp_step() -> Vec<Event> {
    vec![key(KeyCode::Down), key(KeyCode::Char('L')), key(KeyCode::Enter), key(KeyCode::Down), key(KeyCode::Enter)]
}

#[test]
fn the_catalog_is_offered_after_the_reviewer_and_the_run_waits_for_it() {
    let mut backend = fake(&["project-mcp", "outside-in-tdd-mcp"]);

    let lines = show(&mut backend, to_mcp_step());

    assert!(has(&lines, "project-mcp") && has(&lines, "outside-in-tdd-mcp"), "{lines:?}");
    assert!(backend.launched.is_empty());
}

#[test]
fn space_marks_servers_and_enter_saves_them_then_launches() {
    let mut backend = fake(&["project-mcp", "outside-in-tdd-mcp"]);
    let mut events = to_mcp_step();
    events.extend([key(KeyCode::Char(' ')), key(KeyCode::Down), key(KeyCode::Char(' ')), key(KeyCode::Enter)]);

    let lines = show(&mut backend, events);

    assert_eq!(backend.saved, vec![vec!["project-mcp".to_string(), "outside-in-tdd-mcp".to_string()]]);
    assert_eq!(backend.log, vec!["save", "launch"]);
    assert_eq!(backend.launched, vec![("alpha".to_string(), "opus".to_string(), "sonnet".to_string())]);
    assert!(has(&lines, "run-7"), "{lines:?}");
}

#[test]
fn a_server_unmarked_again_is_not_saved() {
    let mut backend = fake(&["project-mcp", "outside-in-tdd-mcp"]);
    let mut events = to_mcp_step();
    events.extend([
        key(KeyCode::Char(' ')),
        key(KeyCode::Down),
        key(KeyCode::Char(' ')),
        key(KeyCode::Char(' ')),
        key(KeyCode::Enter),
    ]);

    show(&mut backend, events);

    assert_eq!(backend.saved, vec![vec!["project-mcp".to_string()]]);
}

#[test]
fn choosing_none_leaves_the_setting_alone_and_still_launches() {
    let mut backend = fake(&["project-mcp"]);
    let mut events = to_mcp_step();
    events.push(key(KeyCode::Enter));

    show(&mut backend, events);

    assert!(backend.saved.is_empty());
    assert_eq!(backend.launched.len(), 1);
}

#[test]
fn an_empty_or_unreadable_catalog_skips_the_step() {
    let mut empty = fake(&[]);
    show(&mut empty, vec![key(KeyCode::Down), key(KeyCode::Char('L')), key(KeyCode::Enter), key(KeyCode::Down), key(KeyCode::Enter)]);
    assert_eq!(empty.launched.len(), 1);
    assert!(empty.saved.is_empty());

    let mut broken = fake(&[]);
    broken.catalog = Err("mcpctl is not installed".into());
    show(&mut broken, vec![key(KeyCode::Down), key(KeyCode::Char('L')), key(KeyCode::Enter), key(KeyCode::Down), key(KeyCode::Enter)]);
    assert_eq!(broken.launched.len(), 1);
}

#[test]
fn a_save_that_fails_stops_the_launch_and_says_why() {
    let mut backend = fake(&["project-mcp"]);
    backend.save_result = Err("cannot write orq/run.toml".into());
    let mut events = to_mcp_step();
    events.extend([key(KeyCode::Char(' ')), key(KeyCode::Enter)]);

    let lines = show(&mut backend, events);

    assert!(backend.launched.is_empty());
    assert!(has(&lines, "cannot write orq/run.toml"), "{lines:?}");
}

#[test]
fn esc_on_the_mcp_step_launches_nothing_and_saves_nothing() {
    let mut backend = fake(&["project-mcp"]);
    let mut events = to_mcp_step();
    events.extend([key(KeyCode::Char(' ')), key(KeyCode::Esc)]);

    show(&mut backend, events);

    assert!(backend.launched.is_empty());
    assert!(backend.saved.is_empty());
}

#[test]
fn a_catalog_longer_than_the_screen_still_shows_the_hint_for_several_marked_features() {
    let names: Vec<String> = (0..40).map(|i| format!("server-{i}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut backend = fake(&refs);
    // Mark two features, then open the picker: the grid line is shown too.
    let mut events = vec![key(KeyCode::Down), key(KeyCode::Char(' ')), key(KeyCode::Down), key(KeyCode::Char(' ')), key(KeyCode::Char('L'))];
    events.extend([key(KeyCode::Enter), key(KeyCode::Down), key(KeyCode::Enter)]);

    let lines = show(&mut backend, events);

    assert!(has(&lines, "Space mark"), "{lines:?}");
}
