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

/// A ledger whose single feature has values the form must not "tidy".
struct Fake {
    stored: FeatureFields,
    edited: Vec<FeatureFields>,
}

impl BrowserBackend for Fake {
    fn load(&mut self) -> Result<Vec<BrowserEntry>, LedgerErrorView> {
        Ok(vec![BrowserEntry {
            name: self.stored.name.clone(),
            description: self.stored.description.clone(),
            status: "draft".into(),
            depends_on: vec![],
            test_file: None,
        }])
    }
    fn promote(&mut self, _feature: &str) -> PromoteOutcome {
        PromoteOutcome::Saved
    }
    fn feature_fields(&mut self, _name: &str) -> Option<FeatureFields> {
        Some(self.stored.clone())
    }
    fn edit_feature(&mut self, fields: &FeatureFields) -> FormSaveOutcome {
        self.edited.push(fields.clone());
        FormSaveOutcome::Saved
    }
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn ctrl_s() -> Event {
    Event::Key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))
}

fn typed(text: &str) -> Vec<Event> {
    text.chars().map(|c| key(KeyCode::Char(c))).collect()
}

fn stored() -> FeatureFields {
    FeatureFields {
        name: " beta ".into(),
        description: Some("padded  ".into()),
        depends_on: vec![],
        parallelizable: Some(true),
        test_file: Some(" tests/b.rs ".into()),
        target_files: vec!["src/a,b.rs".into()],
    }
}

fn run(backend: &mut Fake, mut events: Vec<Event>) {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(TestBackend::new(60, 24)).unwrap();
    // Select the only feature (first Down) and open the edit form.
    let mut script = vec![key(KeyCode::Down), key(KeyCode::Char('e'))];
    script.append(&mut events);
    let _ = run_browser(&mut terminal, &model, backend, &mut Script(script.into()));
}

#[test]
fn saving_without_touching_a_field_hands_its_stored_value_back_unchanged() {
    let mut backend = Fake { stored: stored(), edited: Vec::new() };

    run(&mut backend, vec![ctrl_s()]);

    assert_eq!(backend.edited, vec![stored()]);
}

#[test]
fn a_field_the_user_changed_is_still_tidied_and_the_others_stay_as_stored() {
    let mut backend = Fake { stored: stored(), edited: Vec::new() };
    // Down to Description, append text, save.
    let mut events = vec![key(KeyCode::Down)];
    events.extend(typed("x"));
    events.push(ctrl_s());

    run(&mut backend, events);

    let saved = &backend.edited[0];
    assert_eq!(saved.name, " beta ");
    assert_eq!(saved.description.as_deref(), Some("padded  x"));
    assert_eq!(saved.test_file.as_deref(), Some(" tests/b.rs "));
    assert_eq!(saved.target_files, vec!["src/a,b.rs".to_string()]);
}
