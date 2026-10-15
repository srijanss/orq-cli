use std::collections::VecDeque;
use std::io;

use orq_tui::{
    run_browser, AcceptedCandidate, BrowserBackend, BrowserEntry, CandidateKind, CandidateView, EventSource,
    FormSaveOutcome, LedgerErrorView, PopupModel, PromoteOutcome,
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
    generated: Result<Vec<CandidateView>, String>,
    accept_outcome: Option<FormSaveOutcome>,
    accepted: Vec<Vec<AcceptedCandidate>>,
    saved: Vec<String>,
}

fn view(name: &str, kind: CandidateKind, parallel: bool) -> CandidateView {
    CandidateView {
        name: name.into(),
        description: None,
        depends_on: vec![],
        kind,
        suggests_parallel: parallel,
        ignored_fields: vec![],
    }
}

fn fake() -> Fake {
    Fake {
        generated: Ok(vec![
            view("alpha", CandidateKind::New, true),
            view("beta", CandidateKind::New, false),
            view("base", CandidateKind::Duplicate("already in the feature file".into()), false),
            view("lost", CandidateKind::Invalid("depends on unknown \"ghost\"".into()), false),
        ]),
        accept_outcome: None,
        accepted: Vec::new(),
        saved: Vec::new(),
    }
}

impl BrowserBackend for Fake {
    fn load(&mut self) -> Result<Vec<BrowserEntry>, LedgerErrorView> {
        if self.saved.is_empty() {
            return Err(LedgerErrorView { path: "/repo/.tdd-features.json".into(), reason: "file not found".into() });
        }
        Ok(self
            .saved
            .iter()
            .map(|n| BrowserEntry {
                name: n.clone(),
                description: None,
                status: "draft".into(),
                depends_on: vec![],
                test_file: None,
            })
            .collect())
    }
    fn promote(&mut self, _feature: &str) -> PromoteOutcome {
        PromoteOutcome::Saved
    }
    fn ledger_absent(&self) -> bool {
        self.saved.is_empty()
    }
    fn generate_candidates(&mut self) -> Result<Vec<CandidateView>, String> {
        self.generated.clone()
    }
    fn accept_candidates(&mut self, accepted: &[AcceptedCandidate]) -> FormSaveOutcome {
        self.accepted.push(accepted.to_vec());
        match self.accept_outcome.clone() {
            Some(outcome) => outcome,
            None => {
                self.saved.extend(accepted.iter().map(|a| a.name.clone()));
                FormSaveOutcome::Saved
            }
        }
    }
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn click(column: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

fn screen(backend: &mut Fake, events: Vec<Event>) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let (width, height) = (100u16, 24u16);
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

#[test]
fn the_generate_button_opens_a_preview_of_new_duplicate_and_invalid_candidates() {
    let lines = screen(&mut fake(), vec![]);
    let row = lines.iter().position(|l| l.contains("Generate from Spec")).unwrap();
    let column = lines[row].find("Generate from Spec").unwrap();

    let lines = screen(&mut fake(), vec![click(column as u16, row as u16)]);

    assert!(has(&lines, "2 new, 1 duplicate, 1 invalid"), "{lines:?}");
    assert!(has(&lines, "alpha · parallel"), "{lines:?}");
    assert!(has(&lines, "duplicate: already in the feature file"), "{lines:?}");
    assert!(has(&lines, "invalid: depends on unknown"), "{lines:?}");
    assert!(has(&lines, "enforced later"), "{lines:?}");
}

#[test]
fn enter_adds_only_the_selected_new_candidates_with_the_kept_suggestion() {
    let mut backend = fake();

    let lines = screen(
        &mut backend,
        vec![key(KeyCode::Char('g')), key(KeyCode::Down), key(KeyCode::Char(' ')), key(KeyCode::Enter)],
    );

    assert_eq!(
        backend.accepted,
        vec![vec![AcceptedCandidate { name: "alpha".into(), parallelizable: Some(true) }]]
    );
    assert!(has(&lines, "alpha"), "{lines:?}");
    assert!(!has(&lines, "Generated candidates"), "{lines:?}");
}

#[test]
fn the_parallel_suggestion_can_be_removed_before_adding() {
    let mut backend = fake();

    let _ = screen(&mut backend, vec![key(KeyCode::Char('g')), key(KeyCode::Char('p')), key(KeyCode::Enter)]);

    assert_eq!(backend.accepted[0][0], AcceptedCandidate { name: "alpha".into(), parallelizable: None });
    assert_eq!(backend.accepted[0][1], AcceptedCandidate { name: "beta".into(), parallelizable: None });
}

#[test]
fn reject_all_and_cancel_add_nothing() {
    for close in [KeyCode::Char('x'), KeyCode::Esc] {
        let mut backend = fake();

        let lines = screen(&mut backend, vec![key(KeyCode::Char('g')), key(close)]);

        assert!(backend.accepted.is_empty());
        assert!(has(&lines, "No feature file yet"), "{lines:?}");
    }
}

#[test]
fn a_failed_generation_is_reported_and_manual_creation_stays_usable() {
    let mut backend = fake();
    backend.generated = Err("harness \"pi\" was not found; set one in the Settings tab".into());

    let lines = screen(&mut backend, vec![key(KeyCode::Char('g'))]);

    assert!(has(&lines, "Generation failed"), "{lines:?}");
    assert!(has(&lines, "Settings tab"), "{lines:?}");
    assert!(has(&lines, "Add Feature"), "{lines:?}");
}

#[test]
fn a_failed_save_keeps_the_preview_open_for_a_retry() {
    let mut backend = fake();
    backend.accept_outcome = Some(FormSaveOutcome::Failed { reason: "disk full".into() });

    let lines = screen(&mut backend, vec![key(KeyCode::Char('g')), key(KeyCode::Enter)]);

    assert!(has(&lines, "disk full"), "{lines:?}");
    assert!(has(&lines, "Generated candidates"), "{lines:?}");
}

#[test]
fn nothing_selected_is_not_sent_to_the_backend() {
    let mut backend = fake();

    let lines = screen(
        &mut backend,
        vec![
            key(KeyCode::Char('g')),
            key(KeyCode::Char(' ')),
            key(KeyCode::Down),
            key(KeyCode::Char(' ')),
            key(KeyCode::Enter),
        ],
    );

    assert!(backend.accepted.is_empty());
    assert!(has(&lines, "Select at least one"), "{lines:?}");
}

#[test]
fn the_focused_candidate_shows_its_description_and_dependencies() {
    let mut backend = fake();
    backend.generated = Ok(vec![
        CandidateView {
            name: "alpha".into(),
            description: Some("Read the spec file and list its sections".into()),
            depends_on: vec!["base".into(), "other".into()],
            kind: CandidateKind::New,
            suggests_parallel: false,
            ignored_fields: vec![],
        },
        view("beta", CandidateKind::New, false),
    ]);

    let lines = screen(&mut backend, vec![key(KeyCode::Char('g'))]);

    assert!(has(&lines, "Read the spec file and list its sections"), "{lines:?}");
    assert!(has(&lines, "depends on: base, other"), "{lines:?}");
}
