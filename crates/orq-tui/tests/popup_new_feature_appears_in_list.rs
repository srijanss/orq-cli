use std::collections::VecDeque;
use std::io;

use orq_tui::{
    run_browser, BrowserBackend, BrowserEntry, EventSource, FeatureFields, FormSaveOutcome, LedgerErrorView,
    PopupModel, PromoteOutcome,
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

#[derive(Default)]
struct Fake {
    added: Vec<String>,
}

impl BrowserBackend for Fake {
    fn load(&mut self) -> Result<Vec<BrowserEntry>, LedgerErrorView> {
        let mut names = vec![("alpha".to_string(), "completed")];
        names.extend(self.added.iter().map(|n| (n.clone(), "draft")));
        Ok(names
            .into_iter()
            .map(|(name, status)| BrowserEntry {
                name,
                description: None,
                status: status.into(),
                depends_on: vec![],
                test_file: None,
            })
            .collect())
    }
    fn promote(&mut self, _feature: &str) -> PromoteOutcome {
        PromoteOutcome::Saved
    }
    fn add_feature(&mut self, fields: &FeatureFields) -> FormSaveOutcome {
        self.added.push(fields.name.clone());
        FormSaveOutcome::Saved
    }
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn typed(text: &str) -> Vec<Event> {
    text.chars().map(|c| key(KeyCode::Char(c))).collect()
}

fn run(events: Vec<Event>) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(TestBackend::new(60, 24)).unwrap();
    let _ = run_browser(&mut terminal, &model, &mut Fake::default(), &mut Script(events.into()));
    let buffer = terminal.backend().buffer();
    (0..24)
        .map(|y| (0..60).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
        .collect()
}

fn add_gamma() -> Vec<Event> {
    let mut events = vec![key(KeyCode::Char('a'))];
    events.extend(typed("gamma"));
    events.push(Event::Key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)));
    events
}

fn has(lines: &[String], needle: &str) -> bool {
    lines.iter().any(|l| l.contains(needle))
}

#[test]
fn a_saved_feature_is_selected_and_shown() {
    let lines = run(add_gamma());

    assert!(lines.iter().any(|l| l.contains("gamma") && l.contains("draft")), "{lines:?}");
    assert!(lines.iter().any(|l| l.contains("> ") && l.contains("gamma")), "{lines:?}");
}

#[test]
fn a_saved_feature_hidden_by_the_search_is_named_in_a_notice() {
    let mut events = vec![key(KeyCode::Char('/'))];
    events.extend(typed("alpha"));
    events.push(key(KeyCode::Enter));
    events.extend(add_gamma());

    let lines = run(events);

    assert!(has(&lines, "gamma"), "{lines:?}");
    assert!(has(&lines, "hidden by the search"), "{lines:?}");
}

#[test]
fn a_saved_feature_hidden_by_the_status_filter_is_named_in_a_notice() {
    // Filter to the one non-draft status the file has, then add a draft.
    let mut events = vec![key(KeyCode::Char('f')), key(KeyCode::Down), key(KeyCode::Down), key(KeyCode::Enter)];
    events.extend(add_gamma());

    let lines = run(events);

    assert!(has(&lines, "hidden by the status filter"), "{lines:?}");
}
