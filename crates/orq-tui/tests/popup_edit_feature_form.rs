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

struct Fake {
    outcome: FormSaveOutcome,
    edited: Vec<FeatureFields>,
    added: Vec<FeatureFields>,
    promoted: Vec<String>,
    description: String,
}

impl Fake {
    fn new() -> Fake {
        Fake {
            outcome: FormSaveOutcome::Saved,
            edited: Vec::new(),
            added: Vec::new(),
            promoted: Vec::new(),
            description: "the beta description".into(),
        }
    }
}

impl BrowserBackend for Fake {
    fn load(&mut self) -> Result<Vec<BrowserEntry>, LedgerErrorView> {
        let mut entries = vec![entry("alpha", "completed", None), entry("beta", "draft", Some(&self.description))];
        for added in &self.added {
            entries.push(entry(&added.name, "draft", None));
        }
        Ok(entries)
    }
    fn promote(&mut self, feature: &str) -> PromoteOutcome {
        self.promoted.push(feature.to_string());
        PromoteOutcome::Saved
    }
    fn feature_fields(&mut self, name: &str) -> Option<FeatureFields> {
        (name == "beta").then(|| FeatureFields {
            name: "beta".into(),
            description: Some(self.description.clone()),
            depends_on: vec!["alpha".into()],
            parallelizable: Some(true),
            test_file: None,
            target_files: vec![],
        })
    }
    fn add_feature(&mut self, fields: &FeatureFields) -> FormSaveOutcome {
        self.added.push(fields.clone());
        FormSaveOutcome::Saved
    }
    fn edit_feature(&mut self, fields: &FeatureFields) -> FormSaveOutcome {
        if self.outcome == FormSaveOutcome::Saved {
            self.edited.push(fields.clone());
            if let Some(description) = &fields.description {
                self.description = description.clone();
            }
        }
        self.outcome.clone()
    }
}

fn entry(name: &str, status: &str, description: Option<&str>) -> BrowserEntry {
    BrowserEntry {
        name: name.into(),
        description: description.map(str::to_string),
        status: status.into(),
        depends_on: vec![],
        test_file: None,
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

fn run(backend: &mut Fake, events: Vec<Event>) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let (width, height) = (60u16, 24u16);
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    let _ = run_browser(&mut terminal, &model, backend, &mut Script(events.into()));
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| (0..width).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
        .collect()
}

fn has(lines: &[String], needle: &str) -> bool {
    lines.iter().any(|l| l.contains(needle))
}

/// Selects `beta` (second row; the first Down selects the first) and opens the edit form.
fn open_edit() -> Vec<Event> {
    vec![key(KeyCode::Down), key(KeyCode::Down), key(KeyCode::Char('e'))]
}

#[test]
fn e_opens_the_form_prefilled_with_name_and_status_read_only() {
    let lines = run(&mut Fake::new(), open_edit());

    assert!(has(&lines, "Edit Feature"), "{lines:?}");
    assert!(has(&lines, "beta"), "{lines:?}");
    assert!(has(&lines, "the beta description"), "{lines:?}");
    assert!(has(&lines, "draft"), "{lines:?}");
    assert!(has(&lines, "read-only"), "{lines:?}");
    assert!(has(&lines, "< Yes >"), "{lines:?}");
}

#[test]
fn saving_sends_the_changed_fields_and_never_a_new_name() {
    let mut backend = Fake::new();
    let mut events = open_edit();
    // Typing on the read-only Name row changes nothing.
    events.extend(typed("zzz"));
    events.push(key(KeyCode::Down));
    events.extend(typed(" v2"));
    events.push(ctrl_s());

    let lines = run(&mut backend, events);

    assert_eq!(backend.edited.len(), 1, "{lines:?}");
    assert_eq!(backend.edited[0].name, "beta");
    assert_eq!(backend.edited[0].description.as_deref(), Some("the beta description v2"));
    assert!(!has(&lines, "Depends on"), "form should be closed: {lines:?}");
}

#[test]
fn the_edit_form_does_not_offer_the_feature_as_its_own_dependency() {
    let lines = run(&mut Fake::new(), open_edit());

    let toggles: Vec<_> = lines.iter().filter(|l| l.contains("[x]") || l.contains("[ ]")).collect();
    assert_eq!(toggles.len(), 1, "{lines:?}");
    assert!(toggles[0].contains("alpha") && toggles[0].contains("[x]"), "{lines:?}");
}

#[test]
fn a_conflict_reloads_and_asks_the_user_to_review() {
    let mut backend = Fake::new();
    backend.outcome = FormSaveOutcome::Conflict { reason: "file changed on disk".into() };
    let mut events = open_edit();
    events.push(ctrl_s());

    let lines = run(&mut backend, events);

    assert!(has(&lines, "The feature file changed"), "{lines:?}");
    assert!(!has(&lines, "Depends on"), "{lines:?}");
    assert!(backend.edited.is_empty());
}

#[test]
fn e_works_from_the_detail_view() {
    let events = vec![key(KeyCode::Down), key(KeyCode::Down), key(KeyCode::Enter), key(KeyCode::Char('e'))];

    let lines = run(&mut Fake::new(), events);

    assert!(has(&lines, "Edit Feature"), "{lines:?}");
}

#[test]
fn a_new_feature_is_selected_and_can_be_promoted_with_the_existing_action() {
    let mut backend = Fake::new();
    let mut events = vec![key(KeyCode::Char('a'))];
    events.extend(typed("gamma"));
    events.push(ctrl_s());
    events.extend([key(KeyCode::Char(' ')), key(KeyCode::Char('p'))]);

    run(&mut backend, events);

    assert_eq!(backend.promoted, vec!["gamma".to_string()]);
}

#[test]
fn the_edited_feature_stays_selected_after_the_list_reloads() {
    let mut backend = Fake::new();
    let mut events = open_edit();
    events.push(key(KeyCode::Down));
    events.extend(typed("!"));
    events.push(ctrl_s());

    let lines = run(&mut backend, events);

    assert_eq!(backend.edited.len(), 1, "{lines:?}");
    assert!(lines.iter().any(|l| l.contains("> ") && l.contains("beta")), "{lines:?}");
}
